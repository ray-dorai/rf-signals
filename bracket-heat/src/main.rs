#[macro_use]
extern crate rocket;
use lazy_static::*;
use parking_lot::RwLock;
mod data_defs;
use data_defs::*;
mod calculators;
mod los;
mod tiler;
mod address_layer;
mod catalog;

use rf_signal_algorithms::{set_terrain_path, Frequency, LatLon};
use rocket::fs::NamedFile;
use rocket::http::ContentType;
use rocket::response::content::RawHtml;
use rocket::serde::json::{serde_json, Json};
use rocket::tokio::task::spawn_blocking;
use rocket::State;
use std::path::{Path, PathBuf};

const INDEX_HTML: &str = include_str!("../resources/index.html");
const ADVISOR_HTML: &str = include_str!("../resources/advisor.html");
const ADMIN_HTML: &str = include_str!("../resources/admin.html");

lazy_static! {
    static ref INDEX_FINAL: RwLock<String> = RwLock::new(String::new());
    static ref ADVISOR_FINAL: RwLock<String> = RwLock::new(String::new());
    static ref WISP: RwLock<Wisp> = RwLock::new(Wisp::default());
    /// Normalised antenna patterns, keyed by the `pattern` field on an AP.
    /// Loaded once at startup from `antenna-patterns/` beside isp.ron.
    static ref PATTERNS: RwLock<std::collections::HashMap<String, data_defs::AntennaPattern>> =
        RwLock::new(std::collections::HashMap::new());
}

/// Directory holding index.html, three.js, the marker PNGs and isp.ron.
/// Runtime, not compile time: three instances share one binary.
struct Resources(PathBuf);

fn heat_path() -> String {
    WISP.read().heat_path.clone()
}

/// A rendered PNG tile. Every tiler call is CPU-bound for seconds at a time, so
/// each one runs on the blocking pool; left on the async executor a single
/// signal map would stall every other request on the instance.
type Png = (ContentType, Vec<u8>);

fn png(buffer: Vec<u8>) -> Png {
    (ContentType::PNG, buffer)
}

#[get("/")]
fn index() -> RawHtml<String> {
    RawHtml(INDEX_FINAL.read().clone())
}

#[get("/advisor.html")]
fn advisor() -> RawHtml<String> {
    RawHtml(ADVISOR_FINAL.read().clone())
}

/// Pre-surveyed addresses (parcels) inside the map view, graded for one view:
/// every AP, one technology (`tech`), and/or one service package (`package`).
#[get("/addresses/<swlat>/<swlon>/<nelat>/<nelon>?<limit>&<tech>&<package>")]
fn addresses(swlat: f64, swlon: f64, nelat: f64, nelon: f64, limit: Option<usize>, tech: Option<String>, package: Option<String>)
    -> Result<Json<serde_json::Value>, rocket::http::Status> {
    if ![swlat, swlon, nelat, nelon].iter().all(|v| v.is_finite()) || swlat > nelat || swlon > nelon {
        return Err(rocket::http::Status::BadRequest);
    }
    let cat = catalog::current();
    let tech = tech.filter(|t| !t.is_empty());
    let pkg = match package.filter(|p| !p.is_empty()) {
        Some(id) => Some(cat.packages.iter().find(|p| p.id == id).ok_or(rocket::http::Status::NotFound)?),
        None => None,
    };
    if let Some(t) = &tech {
        if t != catalog::OTHER && !cat.technologies.iter().any(|x| &x.id == t) { return Err(rocket::http::Status::NotFound); }
    }
    let info = address_layer::ap_info(&cat);
    Ok(Json(address_layer::query(swlat, swlon, nelat, nelon, limit.unwrap_or(2500).min(10_000), tech.as_deref(), pkg, &info)))
}

/// Technologies, packages, and which technology each AP falls under.
#[get("/catalog")]
fn get_catalog() -> Json<serde_json::Value> {
    let cat = catalog::current();
    let w = WISP.read();
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let ap_tech: std::collections::BTreeMap<String, String> = w.towers.iter().flat_map(|t| t.access_points.iter().map(|ap| {
        (format!("{}:{}", t.name, ap.name), cat.tech_of(ap).to_string())
    }).collect::<Vec<_>>()).collect();
    for t in ap_tech.values() { *counts.entry(t.clone()).or_default() += 1; }
    Json(serde_json::json!({ "technologies": cat.technologies, "packages": cat.packages, "note": cat.note,
        "ap_tech": ap_tech, "ap_counts": counts, "admin_enabled": admin_password().is_some() }))
}

/// Administrator password, from `BRACKET_HEAT_ADMIN_PASSWORD`. Unset or shorter
/// than 8 characters disables editing entirely.
fn admin_password() -> Option<String> {
    std::env::var("BRACKET_HEAT_ADMIN_PASSWORD").ok().filter(|p| p.len() >= 8)
}

struct Admin;
#[rocket::async_trait]
impl<'r> rocket::request::FromRequest<'r> for Admin {
    type Error = ();
    async fn from_request(req: &'r rocket::Request<'_>) -> rocket::request::Outcome<Self, ()> {
        use rocket::http::Status;
        let Some(want) = admin_password() else { return rocket::request::Outcome::Error((Status::Forbidden, ())) };
        let got = req.headers().get_one("X-Admin-Password").unwrap_or("");
        // constant-time over the longer of the two
        let (a, b) = (want.as_bytes(), got.as_bytes());
        let mut diff = a.len() ^ b.len();
        for i in 0..a.len().max(b.len()) { diff |= (*a.get(i).unwrap_or(&0) ^ *b.get(i).unwrap_or(&0)) as usize; }
        if diff == 0 { rocket::request::Outcome::Success(Admin) } else { rocket::request::Outcome::Error((Status::Unauthorized, ())) }
    }
}

/// Replace the catalog. Administrators only.
#[post("/admin/catalog", format = "json", data = "<body>")]
fn put_catalog(_admin: Admin, body: Json<catalog::Catalog>) -> Result<Json<serde_json::Value>, (rocket::http::Status, String)> {
    catalog::save(&body).map_err(|e| (rocket::http::Status::UnprocessableEntity, e))?;
    Ok(Json(serde_json::json!({ "saved": true, "technologies": body.technologies.len(), "packages": body.packages.len() })))
}

/// Lets the admin page test a password without changing anything.
#[get("/admin/check")]
fn admin_check(_admin: Admin) -> &'static str { "ok" }

#[get("/admin")]
fn admin_page() -> RawHtml<String> {
    RawHtml(ADMIN_HTML.replace("_ISP_NAME_", &WISP.read().name))
}

/// Liveness probe. Reports whether the instance can actually answer, not merely
/// whether the process is up: a server with no terrain under it will return 200
/// for every tile and draw sea level.
#[get("/health")]
fn health() -> Json<serde_json::Value> {
    let w = WISP.read();
    let probe = rf_signal_algorithms::bheat::heat_altitude(
        w.center.0, w.center.1, &w.heat_path);
    Json(serde_json::json!({
        "status": if probe.is_some() { "ok" } else { "no-terrain" },
        "wisp": w.name,
        "towers": w.towers.len(),
        "access_points": w.towers.iter().map(|t| t.access_points.len()).sum::<usize>(),
        "heat_path": w.heat_path,
        "centre_elevation_m": probe.map(|(g, _)| g.as_meters()),
    }))
}

#[get("/<file..>", rank = 9)]
async fn static_file(file: PathBuf, res: &State<Resources>) -> Option<NamedFile> {
    // Serve only the known static assets; never let a path walk the filesystem.
    const ALLOWED: [&str; 4] = ["three.js", "locinfo.html", "tower_Marker.png", "pngegg.png"];
    let name = file.to_str()?;
    if !ALLOWED.contains(&name) {
        return None;
    }
    NamedFile::open(Path::new(&res.0).join(name)).await.ok()
}

#[get("/towers", format = "json")]
fn towers() -> Json<Vec<Tower>> {
    Json(WISP.read().towers.clone())
}

#[get("/budgets", format = "json")]
fn budgets() -> Json<Vec<LinkBudget>> {
    Json(WISP.read().link_budgets.clone())
}

#[get("/heightmap/<swlat>/<swlon>/<nelat>/<nelon>")]
async fn heightmap(swlat: f64, swlon: f64, nelat: f64, nelon: f64) -> Png {
    let hp = heat_path();
    png(spawn_blocking(move || tiler::heightmap_tile(swlat, swlon, nelat, nelon, &hp))
        .await
        .unwrap_or_default())
}

#[get("/heightmap_detail/<swlat>/<swlon>/<nelat>/<nelon>")]
async fn heightmap_detail(swlat: f64, swlon: f64, nelat: f64, nelon: f64) -> Png {
    let hp = heat_path();
    png(spawn_blocking(move || tiler::heightmap_detail(swlat, swlon, nelat, nelon, &hp))
        .await
        .unwrap_or_default())
}

#[get("/losmap/<swlat>/<swlon>/<nelat>/<nelon>/<cpe_height>")]
async fn losmap(swlat: f64, swlon: f64, nelat: f64, nelon: f64, cpe_height: f64) -> Png {
    let hp = heat_path();
    png(
        spawn_blocking(move || tiler::losmap_tile(swlat, swlon, nelat, nelon, cpe_height, &hp))
            .await
            .unwrap_or_default(),
    )
}

#[get("/signalmap/<swlat>/<swlon>/<nelat>/<nelon>/<cpe_height>/<frequency>/<link_budget>")]
async fn signalmap(
    swlat: f64,
    swlon: f64,
    nelat: f64,
    nelon: f64,
    cpe_height: f64,
    frequency: f64,
    link_budget: f64,
) -> Png {
    let hp = heat_path();
    png(spawn_blocking(move || {
        tiler::signalmap_tile(
            swlat, swlon, nelat, nelon, cpe_height, frequency, &hp, link_budget,
        )
    })
    .await
    .unwrap_or_default())
}

#[get("/signalmap_detail/<swlat>/<swlon>/<nelat>/<nelon>")]
async fn signalmap_detail(swlat: f64, swlon: f64, nelat: f64, nelon: f64) -> Png {
    let hp = heat_path();
    png(spawn_blocking(move || tiler::signalmap_detail(swlat, swlon, nelat, nelon, &hp))
        .await
        .unwrap_or_default())
}

#[get(
    "/mapclick/<lat>/<lon>/<cpe_height>/<frequency>/<link_budget>",
    format = "json"
)]
async fn map_click(
    lat: f64,
    lon: f64,
    frequency: f64,
    cpe_height: f64,
    link_budget: f64,
) -> Result<Json<los::ClickSite>, rocket::http::Status> {
    let hp = heat_path();
    // A failed evaluation is an error, not "no towers": unwrap_or_default()
    // used to turn a worker panic into an empty list, which the UI shows as
    // an address nobody can serve.
    spawn_blocking(move || {
        los::evaluate_tower_click(
            &LatLon::new(lat, lon),
            Frequency::with_ghz(frequency),
            cpe_height,
            &hp,
            link_budget,
        )
    })
    .await
    .map(Json)
    .map_err(|_| rocket::http::Status::InternalServerError)
}

/// Evaluate a specific equipment ID, or all APs, at exactly the requested AGL.
#[get("/survey/<lat>/<lon>/<height>?<ap_id>&<cutoff>")]
async fn survey(lat: f64, lon: f64, height: f64, ap_id: Option<String>, cutoff: Option<f64>)
    -> Result<Json<Vec<calculators::SurveyResult>>, rocket::http::Status> {
    let cutoff = cutoff.unwrap_or(-78.0);
    if !lat.is_finite() || !lon.is_finite() || !height.is_finite() || !cutoff.is_finite()
        || lat.abs() > 90.0 || lon.abs() > 180.0 || !(0.5..=100.0).contains(&height)
        || !(-120.0..=-20.0).contains(&cutoff) {
        return Err(rocket::http::Status::BadRequest);
    }
    let hp = heat_path();
    let results = spawn_blocking(move || calculators::survey_at_height(&LatLon::new(lat, lon), height, cutoff, ap_id.as_deref(), &hp))
        .await.map_err(|_| rocket::http::Status::InternalServerError)?;
    if results.is_empty() { return Err(rocket::http::Status::NotFound); }
    Ok(Json(results))
}

#[get("/3d/<lat>/<lon>", format = "json")]
async fn tile3d(lat: f64, lon: f64) -> Json<tiler::TerrainBlob> {
    let hp = heat_path();
    Json(
        spawn_blocking(move || tiler::build_3d_heightmap(lat, lon, &hp))
            .await
            .expect("3d tile task panicked"),
    )
}

#[get("/losplot/<lat>/<lon>/<tower_name>/<cpe_height>/<frequency>")]
async fn los_plot(
    lat: f64,
    lon: f64,
    tower_name: String,
    cpe_height: f64,
    frequency: f64,
) -> Option<Json<los::LineOfSightPlot>> {
    // An unknown tower name used to unwrap a None and take the whole server
    // down. It is a 404.
    let tower_index = WISP
        .read()
        .towers
        .iter()
        .position(|t| t.name == tower_name)?;
    let hp = heat_path();
    let plot = spawn_blocking(move || {
        los::los_plot(
            &LatLon::new(lat, lon),
            tower_index,
            cpe_height,
            Frequency::with_ghz(frequency),
            &hp,
        )
    })
    .await
    .ok()?;
    Some(Json(plot))
}

/// Read a value from the environment, falling back to a default.
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[launch]
fn rocket() -> _ {
    let resources = PathBuf::from(env_or("BRACKET_HEAT_RESOURCES", "resources"));

    // The Google Maps key is read at RUNTIME. It used to be include_str!, which
    // meant the project would not compile at all without the file present and a
    // key rotation meant a rebuild and redeploy of all three instances.
    let key_file = env_or(
        "GMAP_KEY_FILE",
        resources.join("gmap_key.txt").to_string_lossy().as_ref(),
    );
    let gmap_key = std::fs::read_to_string(&key_file)
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|e| {
            eprintln!(
                "WARNING: no Google Maps key at {} ({}). The map will not draw.",
                key_file, e
            );
            String::new()
        });

    let isp_file = env_or(
        "BRACKET_HEAT_ISP",
        resources.join("isp.ron").to_string_lossy().as_ref(),
    );
    let wisp_def = load_wisp(&isp_file);

    // Register the SRTM baseline. Without this a point outside the cooked LiDAR
    // footprint reads as sea level instead of falling back to terrain.
    let terrain = env_or("TERRAIN_PATH", "");
    if terrain.is_empty() {
        eprintln!("WARNING: TERRAIN_PATH unset. Coverage outside cooked .bheat \
                   tiles will be modelled as sea level.");
    }
    set_terrain_path(&terrain);

    let fill = |src: &str| {
        src.replace("_BANNER_", &format!("Bracket-Heat 0.1 - {}", wisp_def.name))
            .replace("_GMAPKEY_", &gmap_key)
            .replace("_CENTER_LAT_", &wisp_def.center.0.to_string())
            .replace("_CENTER_LON_", &wisp_def.center.1.to_string())
            .replace("_MAP_ZOOM_", &wisp_def.map_zoom.to_string())
            .replace("_ISP_NAME_", &format!("\"{}\"", &wisp_def.name))
    };
    address_layer::init(resources.join("address-layer-v2.json"));
    catalog::init(resources.join("packages.json"));
    *INDEX_FINAL.write() = fill(INDEX_HTML);
    *ADVISOR_FINAL.write() = fill(ADVISOR_HTML);

    let port = wisp_def.listen_port;
    println!(
        "{}: {} towers, {} APs, port {}, heat {}, terrain {}",
        wisp_def.name,
        wisp_def.towers.len(),
        wisp_def
            .towers
            .iter()
            .map(|t| t.access_points.len())
            .sum::<usize>(),
        port,
        wisp_def.heat_path,
        if terrain.is_empty() { "NONE" } else { &terrain }
    );
    let pattern_dir = resources.join("antenna-patterns");
    let pats = data_defs::load_patterns(&pattern_dir.to_string_lossy());
    let sectored = wisp_def
        .towers
        .iter()
        .flat_map(|t| t.access_points.iter())
        .filter(|a| a.pattern.is_some() && a.azimuth.is_some())
        .count();
    println!(
        "  antenna patterns: {} loaded from {}; {} APs have a pattern and an azimuth",
        pats.len(),
        pattern_dir.display(),
        sectored
    );
    *PATTERNS.write() = pats;
    *WISP.write() = wisp_def;

    let config = rocket::Config {
        port,
        address: env_or("BRACKET_HEAT_ADDRESS", "127.0.0.1")
            .parse()
            .expect("BRACKET_HEAT_ADDRESS is not an IP address"),
        ..rocket::Config::release_default()
    };

    rocket::custom(config)
        .manage(Resources(resources))
        .mount(
            "/",
            routes![
                index,
                advisor,
                health,
                towers,
                budgets,
                heightmap,
                heightmap_detail,
                losmap,
                signalmap,
                signalmap_detail,
                map_click,
                addresses,
                get_catalog,
                put_catalog,
                admin_check,
                admin_page,
                survey,
                los_plot,
                tile3d,
                static_file,
            ],
        )
}

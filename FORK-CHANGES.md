# What this fork adds

This fork keeps upstream's layout and licence (GPL-2.0) and adds the pieces needed to use
bracket-heat for remote site surveys: deciding, from a desk, whether an address can be served and
from which access point.

## Builds on stable Rust

`bracket-heat` is ported from Rocket 0.4 to 0.5. Slow tile renders run on the blocking pool.

## Configuration is runtime, not compiled in

| Variable | Meaning |
|---|---|
| `BRACKET_HEAT_RESOURCES` | Directory holding `isp.ron`, `gmap_key.txt`, static assets and the optional files below. Default `resources`. One binary can serve several operators from different directories. |
| `BRACKET_HEAT_ISP` / `GMAP_KEY_FILE` | Override the individual file paths. |
| `BRACKET_HEAT_ADDRESS` | Listen address. Default `127.0.0.1`. |
| `TERRAIN_PATH` | SRTM `.hgt` pyramid. Used wherever no cooked LiDAR tile exists. |
| `BRACKET_HEAT_ADMIN_PASSWORD` | Enables saving on `/admin` (8+ characters). Unset = read-only. |
| `RF_PROFILE_CLUTTER` | `1` runs ITWOM over canopy tops instead of bare earth. Default off. |
| `RF_VEG_GAMMA_DB_PER_M`, `RF_VEG_A_MAX_DB` | Optional saturating vegetation loss. Default off; fit before use. |
| `RF_RAIN_R001_MM_H` | Rain rate exceeded 0.01% of the year (ITU-R P.837), for 57-71 GHz. Default 24. |
| `RF_WAVE_AVAILABILITY_PCT` | Availability the reported 57-71 GHz rain fade is sized for. Default 99.9. |

## Terrain and LiDAR fixes

* A missing `.bheat` tile falls back to SRTM instead of reading as sea level.
* SRTM reads are bounds-checked and void-aware; tile edges no longer overrun.
* `bheat_profile` reads many points per lock at decimetre precision.

## ITWOM (Longley-Rice) fixes and diagnostics

* Terrain above 3,000 m AMSL is accepted. The old bound was applied to terrain elevation, which is
  not an ITM input limit, and the resulting panic emptied the whole survey response.
* The profile header counts intervals correctly (the receiver-end sample was being dropped).
* Every parameter-range check records its name and value. Each is marked `blocking` or not,
  following NTIA ITM v1.4: surface refractivity below 250 N-units and paths under 1 km are
  warnings, not errors. `PTPResult::is_valid()` rejects only the errors.

## 57-71 GHz (V-band) links

ITWOM is not valid above 20 GHz. For these APs the planner uses free space plus ITU-R P.676 gas
loss for the AP's actual channel and the path's elevation, and reports the ITU-R P.530/P.838 rain
fade at the configured availability (`rf-signal-algorithms/src/rfcalc/mmwave.rs`). Give each AP its
real channel centre: a nominal `60.0` is treated as "channel unknown" and left at free space.

## Antenna patterns

An AP with `azimuth` and `pattern` gets off-boresight loss from
`<resources>/antenna-patterns/<pattern>.json`:

```json
{ "key": "synth-90deg", "gain_dbi": 17.0, "tilt_deg": 0.0,
  "az": [0.0, 0.1, "... 360 values, dB down from peak, 1 degree steps ..."],
  "el": [0.0, 0.3, "... 360 values ..."], "loss_ceiling_db": 40.0 }
```

`gain_dbi` and `tilt_deg` may be `null`. When a pattern is attached, the AP's `link_budget` must
exclude the AP antenna gain (see `isp-template-extended.ron`).

## LiDAR clear-shot grade and required mount height

Bare-earth ITWOM says "serve" almost everywhere, so every link the survey offers is also graded
against the cooked LiDAR surface (`bracket-heat/src/calculators/clearshot.rs`):

| Grade | Meaning |
|---|---|
| `clear` | 60% of the first Fresnel zone is clear from the address point, mount 3-6 m above ground. |
| `check_mount` | Blocked at the point, but a direct ray is clear from somewhere within 15 m, up to 10 m. |
| `obstructed` | Neither. |
| `unverified` | No cooked LiDAR at one end of the path. |

Each link also carries the lowest clear-shot mount height at the address and within 15 m. Grades
never remove a link. The thresholds were tuned on one operator's install outcomes; check them
against yours.

## Endpoints

* `GET /mapclick/<lat>/<lon>/<max_cpe_height>/<freq>/<budget>` - as before, plus `disposition`,
  `min_mount_agl_m`, `min_mount_nearby_agl_m`, `rssi_basis` and `rain_fade_db` per link. Returns
  500 on an internal failure rather than an empty list.
* `GET /survey/<lat>/<lon>/<height_agl_m>?ap_id=&cutoff=` - one fixed height, every AP or one AP,
  with model warnings and an explicit status. Never an approval.
* `GET /addresses/<swlat>/<swlon>/<nelat>/<nelon>?tech=&package=&limit=` - pre-surveyed addresses
  in view, graded for all APs, one technology or one service package.
* `GET /catalog`, `POST /admin/catalog`, `GET /admin` - technologies and service packages.

## Technologies and service packages

`<resources>/packages.json`, editable at `/admin`:

```json
{ "technologies": [
    { "id": "v-band", "name": "60 GHz", "model_contains": [], "min_ghz": 20.0 },
    { "id": "epmp", "name": "ePMP 5 GHz", "model_contains": ["epmp"], "min_ghz": 4.9, "max_ghz": 7.2 } ],
  "packages": [
    { "id": "100x20", "name": "100 x 20 Mbps", "down_mbps": 100, "up_mbps": 20,
      "rules": [ { "tech": "epmp", "min_rssi_dbm": -65.0, "max_km": null } ] } ] }
```

An AP belongs to the first technology it matches. A package is available at an address when an AP
of an allowed technology is predicted at or above that technology's minimum signal. These are the
operator's rules; the planner does not model throughput or sector load. Without the file, the
technology list is a built-in set of bands and there are no packages.

## Address layer

`<resources>/address-layer-v2.json` holds one row per address with every AP the survey offered
there. Build it by calling `/mapclick` for each address point and writing:

```json
{ "meta": { "version": 2, "cpe_height_m": 4, "generated": "2026-01-01",
            "aps": ["Tower name:AP name", "..."] },
  "rows": [ [39.0101, -95.0203, "100 EXAMPLE ST", "parcel-id", 1,
             [ [0, 0, -61.5, 4.2, 1.35] ] ] ] }
```

Row: lat, lon, address, id, on-building flag, links. Link: index into `meta.aps`, grade
(0 clear, 1 check_mount, 2 unverified, 3 obstructed), RSSI, mount height (m), distance (km). The
file is re-read when it changes; no restart is needed.

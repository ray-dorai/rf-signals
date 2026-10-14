# Reading AP transmit power over SNMP

The planner's link budget is only as good as the transmit side it is given. Inventory systems
usually hold a model name and maybe a nominal power. The radio itself knows what it is actually
set to. These are the objects that report it for three common PMP families. The names come from
the vendors' published MIBs, and every OID was read on production radios. All are scalars (`.0`)
unless noted.

## Cambium PMP 450 / 450i / 450m (WHISP-BOX-MIBV2-MIB)

| OID | Object | Notes |
|-----|--------|-------|
| `.1.3.6.1.4.1.161.19.3.3.1.303.0` | `activeEirp` (dBm) | Answered by every model tested. Use this one. |
| `.1.3.6.1.4.1.161.19.3.3.2.23.0` | `transmitterOP` (dBm, conducted) | 450i only. 450m: no such instance. |
| `.1.3.6.1.4.1.161.19.3.3.2.14.0` | `antennaGain` (dBi) | 450i only. |
| `.1.3.6.1.4.1.161.19.3.3.2.15.0` | `eirp` (dBm) | 450m only. Same value as `activeEirp`. |
| `.1.3.6.1.4.1.161.19.3.3.21.8.0` | `cbsdAntennaAzimuth` (degrees true) | 3 GHz (CBRS) APs only. |
| `.1.3.6.1.4.1.161.19.3.3.21.9.0` | `cbsdAntennaDownTilt` (degrees) | " |
| `.1.3.6.1.4.1.161.19.3.3.21.10.0` | `cbsdAntennaGain` (dBi) | " |
| `.1.3.6.1.4.1.161.19.3.3.21.11.0` | `cbsdAntennaBeamwidth` (degrees) | " |

On the 450i, `activeEirp` = `transmitterOP` + `antennaGain`. The `cbsd*` objects hold the antenna
as registered with the CBRS Spectrum Access System. The operator enters these values, but they
form a second record kept separately from your inventory. Where they disagree with it on
azimuth, one of the two is wrong. A value of exactly 0 is often an unset default.

In isp.ron, an AP with a known EIRP gets `link_budget = EIRP + CPE gain` when it has no
`pattern`. With a pattern of known peak gain, use `EIRP - pattern peak gain + CPE gain`, because
the pattern supplies the antenna and the gain must not be counted twice. Check the result against
measured SM downlink before relying on it. A model's EIRP can be right while its pattern or CPE
gain is not.

## Ubiquiti LTU (UBNT-AFLTU-MIB)

| OID | Object | Notes |
|-----|--------|-------|
| `.1.3.6.1.4.1.41112.1.10.1.2.6.0` | `afLTUTxEIRP` (dBm) | Conducted power plus the gain *typed into the radio*. |
| `.1.3.6.1.4.1.41112.1.10.1.2.7.0` | `afLTUAntennaGain` (dBi) | As configured, not measured. |
| `.1.3.6.1.4.1.41112.1.10.1.2.8.0` | `afLTUCableLoss` (dB) | |

Do not trust the EIRP alone. Radios with the antenna gain left at 0 or 1 dBi report conducted
power as "EIRP". Derive conducted power = EIRP - gain + cable loss, and apply the real
antenna's gain or pattern to that. In the station table, column 15 is `afLTUStaRemoteTxEIRP`
(the station's transmit EIRP), not an SNR.

## Ubiquiti Wave / airFiber 60 (UI-AF60-MIB)

There is no transmit power, EIRP or antenna gain object. Firmware 4.1.x exposes only config,
status, the station table, GPS and orientation, so Wave EIRP has to come from the datasheet.
Useful objects:

| OID | Object | Notes |
|-----|--------|-------|
| `.1.3.6.1.4.1.41112.1.11.1.5.1.1` | `af60OrientationTilt` (degrees) | Accelerometer reading: a measured downtilt. |
| `.1.3.6.1.4.1.41112.1.11.1.4.3.1` | `af60GpsLat` | |
| `.1.3.6.1.4.1.41112.1.11.1.4.4.1` | `af60GpsLon` | |
| station table column 18 | `af60StaRemoteRSSI` (dBm) | Downlink, as heard by the station. |
| station table column 19 | `af60StaRemoteSNR` (dB) | Downlink. |

The station table's local RSSI column is uplink. Use columns 18 and 19 when comparing
predictions with what customers receive.

## Practical notes

* Read power, not just model. Two APs of the same model are often set several dB apart, and the
  difference goes straight into every predicted RSSI.
* Validate each family separately against measured SM downlink before switching it on. A
  correct EIRP can still make predictions worse if the planner's CPE gain or antenna pattern for
  that family is off. Keep the old budget for families where it does.
* Store the source of each AP's budget (`budget_source` in isp.ron), so a bad prediction can be
  traced to an SNMP read or to a family default.

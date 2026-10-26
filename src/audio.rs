//! Headset via the `headsetcontrol` CLI, USB mic via `amixer`.

use anyhow::{Context, Result, bail};
use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("{cmd} not found"))?;
    if !out.status.success() {
        bail!("{cmd}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------- headset

#[derive(Clone, Debug, PartialEq)]
pub enum Battery {
    Level(u8),
    Charging(Option<u8>),
    Off,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Headset {
    pub name: String,
    pub battery: Battery,
    pub sidetone: bool,
    pub inactive_time: bool,
}

pub fn parse_headset(json: &str) -> Result<Option<Headset>> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let Some(d) = v["devices"].as_array().and_then(|a| a.first()) else {
        return Ok(None);
    };
    let level = d["battery"]["level"].as_i64().filter(|l| *l >= 0).map(|l| l as u8);
    let battery = match d["battery"]["status"].as_str() {
        Some("BATTERY_AVAILABLE") => Battery::Level(level.unwrap_or(0)),
        Some("BATTERY_CHARGING") => Battery::Charging(level),
        _ => Battery::Off,
    };
    let caps: Vec<&str> = d["capabilities"]
        .as_array()
        .map(|a| a.iter().filter_map(|c| c.as_str()).collect())
        .unwrap_or_default();
    Ok(Some(Headset {
        name: d["product"].as_str().or(d["device"].as_str()).unwrap_or("Headset").to_string(),
        battery,
        sidetone: caps.contains(&"CAP_SIDETONE"),
        inactive_time: caps.contains(&"CAP_INACTIVE_TIME"),
    }))
}

pub fn headset() -> Result<Option<Headset>> {
    parse_headset(&run("headsetcontrol", &["-b", "-o", "json"])?)
}

/// `flag` is "-s" (sidetone 0-128) or "-i" (auto-off minutes 0-90).
pub fn headset_set(flag: &str, value: u32) -> Result<()> {
    let out = Command::new("headsetcontrol").args([flag, &value.to_string()]).output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

// -------------------------------------------------------------------- mic

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Level {
    pub value: u32,
    pub max: u32,
    pub on: bool,
}

/// First channel of `amixer sget <control>` -> (playback, capture).
pub fn parse_amixer(text: &str) -> (Option<Level>, Option<Level>) {
    let mut max = (None, None);
    let (mut play, mut cap) = (None, None);
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Limits:") {
            let words: Vec<&str> = rest.split_whitespace().collect();
            for w in words.windows(4) {
                let m = w[3].parse().ok();
                match w[0] {
                    "Playback" => max.0 = m,
                    "Capture" => max.1 = m,
                    _ => {}
                }
            }
        } else if (line.starts_with("Front Left:") || line.starts_with("Mono:")) && play.is_none() && cap.is_none() {
            let words: Vec<&str> = line.split_whitespace().collect();
            for (i, w) in words.iter().enumerate() {
                let Some(value) = words.get(i + 1).and_then(|v| v.parse::<u32>().ok()) else { continue };
                // the [on]/[off] switch is the first bracket after the value that says on/off
                let on = words[i + 2..].iter().take(4).find(|s| **s == "[on]" || **s == "[off]") != Some(&"[off]");
                match *w {
                    "Playback" => play = Some(Level { value, max: max.0.unwrap_or(value), on }),
                    "Capture" => cap = Some(Level { value, max: max.1.unwrap_or(value), on }),
                    _ => {}
                }
            }
        }
    }
    (play, cap)
}

/// ALSA card id whose description contains `name`, e.g. "Blue Microphones".
pub fn find_card(name: &str) -> Option<String> {
    let cards = std::fs::read_to_string("/proc/asound/cards").ok()?;
    cards
        .lines()
        .find(|l| l.contains(name))
        .and_then(|l| l.split('[').nth(1))
        .and_then(|l| l.split(']').next())
        .map(|s| s.trim().to_string())
}

pub fn mixer_get(card: &str, control: &str) -> (Option<Level>, Option<Level>) {
    run("amixer", &["-c", card, "sget", control]).map(|t| parse_amixer(&t)).unwrap_or((None, None))
}

/// `value` is a number, or "cap"/"nocap" for the capture switch.
pub fn mixer_set(card: &str, control: &str, direction: &str, value: &str) -> Result<()> {
    run("amixer", &["-c", card, "--", "sset", control, direction, value])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // real output from a Blue Yeti (046d:0ab7)
    const MIC: &str = "Simple mixer control 'Mic',0
  Capabilities: pvolume cvolume pswitch pswitch-joined cswitch cswitch-joined
  Playback channels: Front Left - Front Right
  Capture channels: Front Left - Front Right
  Limits: Playback 0 - 20 Capture 0 - 22
  Front Left: Playback 11 [55%] [-1.33dB] [on] Capture 21 [95%] [19.50dB] [off]
  Front Right: Playback 11 [55%] [-1.33dB] [on] Capture 21 [95%] [19.50dB] [off]";
    const SPEAKER: &str = "Simple mixer control 'Speaker',0
  Limits: Playback 0 - 54
  Mono:
  Front Left: Playback 22 [41%] [-23.74dB] [on]";

    #[test]
    fn amixer() {
        assert_eq!(
            parse_amixer(MIC),
            (Some(Level { value: 11, max: 20, on: true }), Some(Level { value: 21, max: 22, on: false }))
        );
        assert_eq!(parse_amixer(SPEAKER), (Some(Level { value: 22, max: 54, on: true }), None));
        assert_eq!(parse_amixer(""), (None, None));
    }

    #[test]
    fn headset_json() {
        // trimmed real output from an HS80 MAX
        let real = r#"{"devices":[{"status":"success","device":"Corsair Wireless V2 Headset Device",
            "product":"CORSAIR HS80 MAX WIRELESS Gaming Receiver",
            "capabilities":["CAP_SIDETONE","CAP_BATTERY_STATUS","CAP_INACTIVE_TIME"],
            "battery":{"status":"BATTERY_AVAILABLE","level":48}}]}"#;
        let h = parse_headset(real).unwrap().unwrap();
        assert_eq!(h.battery, Battery::Level(48));
        assert!(h.sidetone && h.inactive_time);
        assert_eq!(h.name, "CORSAIR HS80 MAX WIRELESS Gaming Receiver");

        let charging = r#"{"devices":[{"battery":{"status":"BATTERY_CHARGING","level":-1}}]}"#;
        assert_eq!(parse_headset(charging).unwrap().unwrap().battery, Battery::Charging(None));
        let off = r#"{"devices":[{"battery":{"status":"BATTERY_UNAVAILABLE"}}]}"#;
        assert_eq!(parse_headset(off).unwrap().unwrap().battery, Battery::Off);
        assert_eq!(parse_headset(r#"{"devices":[]}"#).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn failed_command_is_an_error() {
        assert!(run("sh", &["-c", "exit 1"]).is_err());
    }
}

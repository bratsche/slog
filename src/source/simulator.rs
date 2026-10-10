//! Booted iOS, watchOS, tvOS and visionOS simulators, via `xcrun simctl`.
//!
//! A simulator has its own unified log, separate from the Mac's. `/usr/bin/log` inside it
//! (`simctl spawn UDID log ...`) produces the same ndjson as on the Mac, so `mac::run`
//! parses it; only the command differs.

use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

const XCRUN: &str = "/usr/bin/xcrun";

#[derive(Debug, Serialize)]
pub struct SimulatorInfo {
    pub udid: String,
    pub name: String,
    /// Runtime, e.g. "iOS 26.5".
    pub os: String,
}

#[derive(Deserialize)]
struct SimctlList {
    devices: std::collections::BTreeMap<String, Vec<SimctlDevice>>,
}

#[derive(Deserialize)]
struct SimctlDevice {
    udid: String,
    name: String,
    state: String,
}

/// Every booted simulator, in `simctl`'s runtime order.
pub async fn list() -> Result<Vec<SimulatorInfo>> {
    let out = Command::new(XCRUN)
        .args(["simctl", "list", "devices", "booted", "--json"])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        // Own process group, so a terminal Ctrl-C reaches only slog (see `mac::run`).
        .process_group(0)
        .output()
        .await
        .with_context(|| format!("failed to run {XCRUN} simctl"))?;
    if !out.status.success() {
        bail!(
            "{XCRUN} simctl list failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    parse_list(&out.stdout)
}

fn parse_list(json: &[u8]) -> Result<Vec<SimulatorInfo>> {
    let list: SimctlList =
        serde_json::from_slice(json).context("unexpected output from simctl list")?;
    Ok(list
        .devices
        .into_iter()
        .flat_map(|(runtime, devices)| {
            let os = runtime_name(&runtime);
            devices
                .into_iter()
                .filter(|d| d.state == "Booted")
                .map(move |d| SimulatorInfo {
                    udid: d.udid,
                    name: d.name,
                    os: os.clone(),
                })
        })
        .collect())
}

/// `com.apple.CoreSimulator.SimRuntime.iOS-26-5` -> `iOS 26.5`.
fn runtime_name(id: &str) -> String {
    let short = id.rsplit('.').next().unwrap_or(id);
    match short.split_once('-') {
        Some((os, version)) => format!("{os} {}", version.replace('-', ".")),
        None => short.to_string(),
    }
}

/// The UDID of the booted simulator `selector` names (UDID or name, case-insensitive);
/// an empty selector means the only booted simulator.
pub async fn select(selector: &str) -> Result<String> {
    pick(list().await?, selector)
}

fn pick(booted: Vec<SimulatorInfo>, selector: &str) -> Result<String> {
    let describe = |sims: &[&SimulatorInfo]| {
        sims.iter()
            .map(|s| format!("{} ({}, {})", s.udid, s.name, s.os))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let matches: Vec<&SimulatorInfo> = booted
        .iter()
        .filter(|s| {
            selector.is_empty()
                || s.udid.eq_ignore_ascii_case(selector)
                || s.name.eq_ignore_ascii_case(selector)
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.udid.clone()),
        [] if booted.is_empty() => Err(anyhow!(
            "no booted simulators; boot one in Xcode or with `xcrun simctl boot NAME`"
        )),
        [] => Err(anyhow!(
            "no booted simulator matches {selector:?}; booted: {}",
            describe(&booted.iter().collect::<Vec<_>>())
        )),
        several => Err(anyhow!(
            "{} booted simulators match; pass a UDID with --simulator: {}",
            several.len(),
            describe(several)
        )),
    }
}

/// `log` run inside the simulator; the caller adds its arguments.
pub fn log_command(udid: &str) -> Command {
    let mut cmd = Command::new(XCRUN);
    cmd.args(["simctl", "spawn", udid, "log"]);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"{"devices":{
      "com.apple.CoreSimulator.SimRuntime.iOS-18-5":[],
      "com.apple.CoreSimulator.SimRuntime.iOS-26-5":[
        {"udid":"712F2157-7600-45DD-9DD6-31C2EE090D99","name":"iPhone 17 Pro","state":"Booted","isAvailable":true},
        {"udid":"52C6E756-DBC4-4CB7-A4D6-CFAF35E57396","name":"iPhone 17 Pro Max","state":"Shutdown","isAvailable":true}],
      "com.apple.CoreSimulator.SimRuntime.watchOS-26-0":[
        {"udid":"A0CA6B74-E01C-4B1F-971B-1B110E920E5B","name":"Apple Watch Series 11 (46mm)","state":"Booted","isAvailable":true}]
    }}"#;

    #[test]
    fn lists_booted_simulators_only() {
        let sims = parse_list(LIST.as_bytes()).unwrap();
        let summary: Vec<_> = sims
            .iter()
            .map(|s| (s.name.as_str(), s.os.as_str()))
            .collect();
        assert_eq!(
            summary,
            [
                ("iPhone 17 Pro", "iOS 26.5"),
                ("Apple Watch Series 11 (46mm)", "watchOS 26.0")
            ]
        );
    }

    #[test]
    fn selects_by_udid_name_or_only_one() {
        let sims = || parse_list(LIST.as_bytes()).unwrap();
        assert!(
            pick(sims(), "")
                .unwrap_err()
                .to_string()
                .contains("2 booted")
        );
        assert_eq!(
            pick(sims(), "iphone 17 pro").unwrap(),
            "712F2157-7600-45DD-9DD6-31C2EE090D99"
        );
        assert_eq!(
            pick(sims(), "a0ca6b74-e01c-4b1f-971b-1b110e920e5b").unwrap(),
            "A0CA6B74-E01C-4B1F-971B-1B110E920E5B"
        );
        assert!(
            pick(sims(), "iPad")
                .unwrap_err()
                .to_string()
                .contains("booted: ")
        );
        assert!(
            pick(vec![], "")
                .unwrap_err()
                .to_string()
                .contains("simctl boot")
        );
    }
}

//! iOS, watchOS, tvOS and visionOS simulator devices and runtimes.
//!
//! Both are discovered with `xcrun simctl` and removed with it, never by
//! deleting files: CoreSimulator keeps its own registry of devices, and a
//! runtime is a mounted disk image that only simctl can detach and delete.
//! A device or runtime that is anything but shut down is never removed.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::CacheCandidate;
use crate::native::{self, AgeSignal, Planned, Removal, Resolved, Sizing, Target};

pub(crate) const DEVICE_KIND: &str = "xcode-simulator-device";
pub(crate) const RUNTIME_KIND: &str = "xcode-simulator-runtime";
const ECOSYSTEM: &str = "xcode";
const SHUTDOWN: &str = "Shutdown";
const RUNTIME_KEY_PREFIX: &str = "com.apple.CoreSimulator.SimRuntime.";
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const DEVICE_DELETE_TIMEOUT: Duration = Duration::from_secs(120);
const RUNTIME_DELETE_TIMEOUT: Duration = Duration::from_secs(600);

/// A device that holds less than this has nothing worth recovering: a freshly
/// created simulator is a few megabytes and is recreated on demand.
const MINIMUM_DEVICE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
struct Device {
    name: String,
    /// The reported state; empty when simctl gave none, which never reads as
    /// shut down.
    state: String,
    /// The runtime key the device is listed under.
    runtime_key: String,
    /// Where the device can be acted on. Absent when its identifier or data
    /// path is missing or untrusted: such a device is never offered for
    /// deletion but still counts as using its runtime.
    home: Option<DeviceHome>,
}

#[derive(Debug, PartialEq, Eq)]
struct DeviceHome {
    udid: String,
    directory: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Runtime {
    identifier: String,
    runtime_identifier: String,
    path: PathBuf,
    build: Option<String>,
    version: Option<String>,
    size_bytes: u64,
    deletable: bool,
    last_used_unix: Option<u64>,
}

pub(crate) fn plan_devices() -> (Vec<Planned>, Vec<String>) {
    match query_devices() {
        Ok(devices) => (devices.iter().filter_map(device_plan).collect(), Vec::new()),
        Err(unavailable) => (
            Vec::new(),
            unavailable
                .warning("simctl", "simulator devices are not offered")
                .into_iter()
                .collect(),
        ),
    }
}

pub(crate) fn plan_runtimes() -> (Vec<Planned>, Vec<String>) {
    match query_runtimes() {
        Ok((runtimes, warnings)) => {
            // Without the device list nothing proves a runtime is unused, so
            // none is offered as cleanable.
            let devices = query_devices().ok();
            (
                runtimes
                    .iter()
                    .map(|runtime| runtime_plan(runtime, &runtimes, devices.as_deref()))
                    .collect(),
                warnings,
            )
        }
        Err(unavailable) => (
            Vec::new(),
            unavailable
                .warning("simctl", "simulator runtimes are not offered")
                .into_iter()
                .collect(),
        ),
    }
}

/// Re-query the device and require it to be shut down right now.
pub(crate) fn resolve_device(candidate: &CacheCandidate) -> Result<Resolved, String> {
    let devices = query_devices().map_err(|unavailable| unavailable.message("simctl"))?;
    let device = devices
        .into_iter()
        .find(|device| {
            device
                .home
                .as_ref()
                .is_some_and(|home| home.directory == candidate.path)
        })
        .ok_or_else(|| "simulator device no longer exists".to_owned())?;
    if device.state != SHUTDOWN {
        return Err(format!(
            "simulator device {} is {}, not shut down",
            device.name,
            device.state.to_lowercase()
        ));
    }
    let planned =
        device_plan(&device).ok_or_else(|| "simulator device no longer exists".to_owned())?;
    let home = device
        .home
        .ok_or_else(|| "simulator device no longer exists".to_owned())?;
    Ok(Resolved {
        target: Target::Device(home.udid),
        planned,
    })
}

/// Re-query the runtime and require that it is deletable and that no device
/// running on it is booted or changing state.
pub(crate) fn resolve_runtime(candidate: &CacheCandidate) -> Result<Resolved, String> {
    let (runtimes, _) = query_runtimes().map_err(|unavailable| unavailable.message("simctl"))?;
    let runtime = runtimes
        .iter()
        .find(|runtime| runtime.path == candidate.path)
        .ok_or_else(|| "simulator runtime is no longer installed".to_owned())?;
    if !runtime.deletable {
        return Err("simctl reports this simulator runtime as not deletable".to_owned());
    }
    let devices = query_devices().map_err(|unavailable| unavailable.message("simctl"))?;
    if let Some(running) = device_using(runtime, &devices) {
        return Err(format!(
            "simulator device {} is {} and uses this runtime",
            running.name,
            running.state.to_lowercase()
        ));
    }
    let planned = runtime_plan(runtime, &runtimes, Some(&devices));
    Ok(Resolved {
        target: Target::Runtime(runtime.identifier.clone()),
        planned,
    })
}

pub(crate) fn delete_device(udid: &str, directory: &Path) -> Result<(), String> {
    native::run_tool(
        "xcrun",
        &["simctl", "delete", udid],
        DEVICE_DELETE_TIMEOUT,
        "simctl delete",
    )
    .map_err(|unavailable| unavailable.message("xcrun"))?;
    if std::fs::symlink_metadata(directory).is_ok() {
        return Err("simctl reported success but the device directory is still present".into());
    }
    Ok(())
}

/// Runtime removal is handled by CoreSimulator's own service, which may finish
/// detaching the image after the command returns. The runtime list is queried
/// again afterwards: only a runtime that is no longer listed is a completed
/// removal, anything else (still listed, or the list is unavailable) is
/// reported as deferred.
pub(crate) fn delete_runtime(identifier: &str) -> Result<Removal, String> {
    native::run_tool(
        "xcrun",
        &["simctl", "runtime", "delete", identifier],
        RUNTIME_DELETE_TIMEOUT,
        "simctl runtime delete",
    )
    .map_err(|unavailable| unavailable.message("xcrun"))?;
    Ok(match list_runtimes() {
        Ok(listing) if runtime_absent(&listing.stdout_text(), identifier) => Removal::Complete,
        _ => Removal::Deferred,
    })
}

/// True only when the raw listing parses and the identifier appears neither as
/// a key nor as any entry's `identifier`, so an entry the typed parser would
/// skip still counts as present.
fn runtime_absent(listing: &str, identifier: &str) -> bool {
    let Ok(Value::Object(entries)) = serde_json::from_str::<Value>(listing) else {
        return false;
    };
    entries.iter().all(|(key, entry)| {
        key != identifier && entry.get("identifier").and_then(Value::as_str) != Some(identifier)
    })
}

/// A device on the runtime that is not shut down.
fn device_using<'a>(runtime: &Runtime, devices: &'a [Device]) -> Option<&'a Device> {
    devices
        .iter()
        .find(|device| device.runtime_key == runtime.runtime_identifier && device.state != SHUTDOWN)
}

fn device_plan(device: &Device) -> Option<Planned> {
    let home = device.home.as_ref()?;
    Some(Planned {
        kind: DEVICE_KIND,
        ecosystem: ECOSYSTEM,
        path: home.directory.clone(),
        label: Some(format!(
            "{} ({})",
            device.name,
            runtime_name(&device.runtime_key)
        )),
        network_restore: false,
        cleanable: device.state == SHUTDOWN,
        unprotected_because: None,
        sizing: Sizing::Tree {
            age: AgeSignal::DeviceData,
            minimum_bytes: MINIMUM_DEVICE_BYTES,
        },
    })
}

/// `runtimes` is every installed runtime; `devices` is the current device
/// list, or `None` when it is unavailable.
fn runtime_plan(runtime: &Runtime, runtimes: &[Runtime], devices: Option<&[Device]>) -> Planned {
    let label = match (&runtime.version, &runtime.build) {
        (Some(version), Some(build)) => format!(
            "{} {version} ({build})",
            runtime_platform(&runtime.runtime_identifier)
        ),
        _ => runtime_name(&runtime.runtime_identifier),
    };
    Planned {
        kind: RUNTIME_KIND,
        ecosystem: ECOSYSTEM,
        path: runtime.path.clone(),
        label: Some(label),
        network_restore: true,
        cleanable: runtime.deletable
            && devices.is_some_and(|devices| device_using(runtime, devices).is_none()),
        unprotected_because: devices
            .and_then(|devices| superseded(runtime, runtimes, devices, native::has_installed_apps)),
        sizing: Sizing::Reported {
            bytes: runtime.size_bytes,
            last_used_unix: runtime.last_used_unix,
        },
    }
}

/// Why a runtime can go whatever its recorded use: a newer runtime for the
/// same platform is installed, and every device on this one is shut down and
/// holds no installed app. Xcode creates a default set of devices for each
/// runtime, so a runtime that was never used still has devices; an empty
/// device is recreated on demand and holds nothing to lose. `None` when any
/// device on the runtime is running, holds apps, or has an untrusted data
/// path, or when no newer runtime for the platform is installed.
fn superseded(
    runtime: &Runtime,
    runtimes: &[Runtime],
    devices: &[Device],
    holds_apps: impl Fn(&Path) -> bool,
) -> Option<String> {
    let version = runtime_version(runtime)?;
    let platform = runtime_platform(&runtime.runtime_identifier);
    let newer = runtimes
        .iter()
        .filter(|other| runtime_platform(&other.runtime_identifier) == platform)
        .filter_map(|other| Some((runtime_version(other)?, other)))
        .filter(|(other_version, _)| *other_version > version)
        .max_by(|(left, _), (right, _)| left.cmp(right))?
        .1;
    let on_runtime: Vec<&Device> = devices
        .iter()
        .filter(|device| device.runtime_key == runtime.runtime_identifier)
        .collect();
    let all_empty = on_runtime.iter().all(|device| {
        device.state == SHUTDOWN
            && device
                .home
                .as_ref()
                .is_some_and(|home| !holds_apps(&home.directory))
    });
    if !all_empty {
        return None;
    }
    let newer_name = runtime_name(&newer.runtime_identifier);
    Some(match on_runtime.len() {
        0 => format!("superseded by {newer_name}; no simulator device uses it"),
        1 => format!("superseded by {newer_name}; its one simulator device holds no apps"),
        count => {
            format!("superseded by {newer_name}; its {count} simulator devices hold no apps")
        }
    })
}

/// The runtime's dotted version as numbers, from the reported version or else
/// from the runtime identifier (`iOS-26-5`). `None` when neither parses.
fn runtime_version(runtime: &Runtime) -> Option<Vec<u64>> {
    let parse = |text: &str, separator: char| -> Option<Vec<u64>> {
        text.split(separator)
            .map(|part| part.parse().ok())
            .collect::<Option<Vec<u64>>>()
            .filter(|parts| !parts.is_empty())
    };
    runtime
        .version
        .as_deref()
        .and_then(|version| parse(version, '.'))
        .or_else(|| {
            let short = runtime
                .runtime_identifier
                .strip_prefix(RUNTIME_KEY_PREFIX)
                .unwrap_or(&runtime.runtime_identifier);
            parse(short.split_once('-')?.1, '-')
        })
}

/// `com.apple.CoreSimulator.SimRuntime.iOS-26-5` reads as `iOS 26.5`.
fn runtime_name(key: &str) -> String {
    let short = key.strip_prefix(RUNTIME_KEY_PREFIX).unwrap_or(key);
    match short.split_once('-') {
        Some((platform, version)) if !platform.is_empty() && !version.is_empty() => {
            format!("{platform} {}", version.replace('-', "."))
        }
        _ => short.to_owned(),
    }
}

fn runtime_platform(key: &str) -> String {
    let short = key.strip_prefix(RUNTIME_KEY_PREFIX).unwrap_or(key);
    short.split('-').next().unwrap_or(short).to_owned()
}

fn query_devices() -> Result<Vec<Device>, native::Unavailable> {
    let output = native::run_tool(
        "xcrun",
        &["simctl", "list", "devices", "--json"],
        LIST_TIMEOUT,
        "simctl list devices",
    )?;
    parse_devices(&output.stdout_text()).map_err(native::Unavailable::Problem)
}

fn list_runtimes() -> Result<crate::process::Output, native::Unavailable> {
    native::run_tool(
        "xcrun",
        &["simctl", "runtime", "list", "-j"],
        LIST_TIMEOUT,
        "simctl runtime list",
    )
}

fn query_runtimes() -> Result<(Vec<Runtime>, Vec<String>), native::Unavailable> {
    let output = list_runtimes()?;
    parse_runtimes(&output.stdout_text()).map_err(native::Unavailable::Problem)
}

/// A device directory is `<devices root>/<udid>`; `dataPath` is its `data`
/// child. Entries whose paths do not have that shape are not trusted.
fn parse_devices(json: &str) -> Result<Vec<Device>, String> {
    let value: Value = serde_json::from_str(json)
        .map_err(|error| format!("simctl list devices returned invalid JSON: {error}"))?;
    let groups = value
        .get("devices")
        .and_then(Value::as_object)
        .ok_or("simctl list devices JSON has no devices object")?;
    let mut devices = Vec::new();
    for (runtime_key, group) in groups {
        let Some(group) = group.as_array() else {
            continue;
        };
        // Every entry counts as a device on the runtime, whatever else it is
        // missing: runtime safety depends on states, not on trusted paths.
        for entry in group {
            let text = |field: &str| entry.get(field).and_then(Value::as_str);
            devices.push(Device {
                name: text("name").unwrap_or("an unnamed device").to_owned(),
                state: text("state").unwrap_or_default().to_owned(),
                runtime_key: runtime_key.clone(),
                home: device_home(text("udid"), text("dataPath")),
            });
        }
    }
    devices.sort_by(|left, right| {
        let directory = |device: &Device| device.home.as_ref().map(|home| home.directory.clone());
        directory(left).cmp(&directory(right))
    });
    Ok(devices)
}

/// A device directory is `<devices root>/<udid>`; `dataPath` is its `data`
/// child. Anything else is not trusted.
fn device_home(udid: Option<&str>, data_path: Option<&str>) -> Option<DeviceHome> {
    let (udid, data_path) = (udid?, Path::new(data_path?));
    let directory = data_path.parent()?;
    let well_formed = valid_udid(udid)
        && data_path.is_absolute()
        && data_path.file_name().is_some_and(|leaf| leaf == "data")
        && directory.file_name().is_some_and(|leaf| leaf == udid);
    well_formed.then(|| DeviceHome {
        udid: udid.to_owned(),
        directory: directory.to_path_buf(),
    })
}

/// Runtimes without a reported size or an image path are skipped with a
/// warning rather than listed with an invented size.
fn parse_runtimes(json: &str) -> Result<(Vec<Runtime>, Vec<String>), String> {
    let value: Value = serde_json::from_str(json)
        .map_err(|error| format!("simctl runtime list returned invalid JSON: {error}"))?;
    let entries = value
        .as_object()
        .ok_or("simctl runtime list JSON is not an object")?;
    let mut runtimes = Vec::new();
    let mut warnings = Vec::new();
    for (key, entry) in entries {
        let text = |field: &str| entry.get(field).and_then(Value::as_str);
        let identifier = text("identifier").unwrap_or(key);
        let path = text("path")
            .or_else(|| text("mountPath"))
            .map(Path::new)
            .filter(|path| path.is_absolute());
        let size = entry.get("sizeBytes").and_then(Value::as_u64);
        let (Some(runtime_identifier), Some(path), Some(size_bytes), true) = (
            text("runtimeIdentifier"),
            path,
            size,
            valid_udid(identifier),
        ) else {
            warnings.push(format!(
                "simctl: runtime {} has no usable identifier, path or size and is not offered",
                native_safe(identifier)
            ));
            continue;
        };
        runtimes.push(Runtime {
            identifier: identifier.to_owned(),
            runtime_identifier: runtime_identifier.to_owned(),
            path: path.to_path_buf(),
            build: text("build").map(str::to_owned),
            version: text("version").map(str::to_owned),
            size_bytes,
            deletable: entry
                .get("deletable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            last_used_unix: text("lastUsedAt").and_then(parse_utc_timestamp),
        });
    }
    runtimes.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((runtimes, warnings))
}

fn native_safe(text: &str) -> String {
    crate::process::sanitize_message(text)
}

/// Identifiers reach an argv, so only the canonical UUID shape is accepted.
fn valid_udid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.chars().all(|c| c.is_ascii_hexdigit())
        })
}

/// Parse `2026-10-08T21:00:32Z` (with optional fractional seconds) into Unix
/// seconds. Any other shape is unknown rather than guessed.
fn parse_utc_timestamp(text: &str) -> Option<u64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let time = time.split_once('.').map_or(time, |(whole, fraction)| {
        if fraction.chars().all(|c| c.is_ascii_digit()) {
            whole
        } else {
            ""
        }
    });
    let number = |part: &str, width: usize| -> Option<u64> {
        (part.len() == width && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())?
    };
    let mut date = date.split('-');
    let (year, month, day) = (
        number(date.next()?, 4)?,
        number(date.next()?, 2)?,
        number(date.next()?, 2)?,
    );
    let mut time = time.split(':');
    let (hour, minute, second) = (
        number(time.next()?, 2)?,
        number(time.next()?, 2)?,
        number(time.next()?, 2)?,
    );
    if date.next().is_some()
        || time.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days since 1970-01-01 for a proleptic Gregorian date.
    let year_shifted = if month <= 2 { year - 1 } else { year };
    let era = year_shifted / 400;
    let year_of_era = year_shifted - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = (era * 146_097 + day_of_era).checked_sub(719_468)?;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    const IOS_18_2: &str = "com.apple.CoreSimulator.SimRuntime.iOS-18-2";
    const WATCHOS_26_5: &str = "com.apple.CoreSimulator.SimRuntime.watchOS-26-5";

    fn trusted_devices(devices: Vec<Device>) -> Vec<Device> {
        devices
            .into_iter()
            .filter(|device| device.home.is_some())
            .collect()
    }

    fn devices_json() -> String {
        format!(
            r#"{{"devices":{{
  "{IOS_18_2}":[
    {{"dataPath":"/Users/example/Library/Developer/CoreSimulator/Devices/2B5F8808-A600-48F0-9750-81CCBA294022/data","dataPathSize":18337792,"logPath":"/Users/example/Library/Logs/CoreSimulator/2B5F8808-A600-48F0-9750-81CCBA294022","udid":"2B5F8808-A600-48F0-9750-81CCBA294022","isAvailable":true,"deviceTypeIdentifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-16-Pro","state":"Shutdown","name":"iPhone 16 Pro"}},
    {{"dataPath":"/Users/example/Library/Developer/CoreSimulator/Devices/7A1C3F10-0000-4000-8000-000000000001/data","udid":"7A1C3F10-0000-4000-8000-000000000001","isAvailable":true,"state":"Booted","name":"iPhone 16"}}
  ],
  "{WATCHOS_26_5}":[],
  "com.apple.CoreSimulator.SimRuntime.tvOS-18-0":[
    {{"dataPath":"/Users/example/elsewhere/data","udid":"7A1C3F10-0000-4000-8000-000000000002","state":"Shutdown","name":"Apple TV"}},
    {{"dataPath":"/Users/example/Library/Developer/CoreSimulator/Devices/not-a-udid/data","udid":"not-a-udid","state":"Shutdown","name":"Broken"}},
    {{"udid":"7A1C3F10-0000-4000-8000-000000000003","state":"Shutdown","name":"No path"}}
  ]
}}}}"#
        )
    }

    fn runtimes_json() -> String {
        r#"{
  "5C29E7B8-A172-4CC5-B491-8F4DDA3C9AB2":{"build":"22S99","deletable":true,"identifier":"5C29E7B8-A172-4CC5-B491-8F4DDA3C9AB2","kind":"Cryptex Disk Image","mountPath":"/Library/Developer/CoreSimulator/Volumes/watchOS_22S99","path":"/System/Library/AssetsV2/example/AssetData/Restore/one.dmg","runtimeIdentifier":"com.apple.CoreSimulator.SimRuntime.watchOS-11-2","sizeBytes":4541193378,"state":"Ready","version":"11.2"},
  "15CCF121-0865-4533-B65E-582CB34C5835":{"build":"23T570","deletable":true,"identifier":"15CCF121-0865-4533-B65E-582CB34C5835","lastUsedAt":"2026-09-28T14:21:58Z","path":"/System/Library/AssetsV2/example/AssetData/Restore/two.dmg","runtimeIdentifier":"com.apple.CoreSimulator.SimRuntime.watchOS-26-5","sizeBytes":3935033546,"state":"Ready","version":"26.5"},
  "38BB7EA4-FB30-4597-A6EA-C38FEC38B89C":{"build":"23F77","deletable":false,"identifier":"38BB7EA4-FB30-4597-A6EA-C38FEC38B89C","lastUsedAt":"2026-10-08T21:00:32Z","path":"/System/Library/AssetsV2/example/AssetData/Restore/three.dmg","runtimeIdentifier":"com.apple.CoreSimulator.SimRuntime.iOS-26-5","sizeBytes":8494282293,"state":"Ready","version":"26.5"}
}"#
        .to_owned()
    }

    #[test]
    fn devices_are_read_from_the_dictionary_with_trusted_paths_only() {
        let devices = parse_devices(&devices_json()).unwrap();
        let trusted: Vec<&str> = devices
            .iter()
            .filter(|device| device.home.is_some())
            .map(|device| device.name.as_str())
            .collect();
        assert_eq!(trusted, ["iPhone 16 Pro", "iPhone 16"]);
        assert_eq!(devices.len(), 5, "untrusted entries still count as devices");
        let devices = trusted_devices(devices);
        assert_eq!(
            devices[0].home.as_ref().unwrap().directory,
            Path::new(
                "/Users/example/Library/Developer/CoreSimulator/Devices/2B5F8808-A600-48F0-9750-81CCBA294022"
            )
        );
        assert_eq!(devices[0].runtime_key, IOS_18_2);
        assert_eq!(devices[1].state, "Booted");
    }

    #[test]
    fn device_labels_name_the_runtime_derived_from_the_key() {
        let devices = trusted_devices(parse_devices(&devices_json()).unwrap());
        assert_eq!(
            device_plan(&devices[0]).unwrap().label.as_deref(),
            Some("iPhone 16 Pro (iOS 18.2)")
        );
        assert_eq!(runtime_name(WATCHOS_26_5), "watchOS 26.5");
        assert_eq!(runtime_name("unrecognized"), "unrecognized");
        assert_eq!(runtime_name(""), "");
    }

    #[test]
    fn device_plans_use_the_device_data_as_the_age_signal() {
        let devices = trusted_devices(parse_devices(&devices_json()).unwrap());
        let planned = device_plan(&devices[0]).unwrap();
        assert_eq!(
            planned.sizing,
            Sizing::Tree {
                age: AgeSignal::DeviceData,
                minimum_bytes: MINIMUM_DEVICE_BYTES
            }
        );
        assert!(!planned.network_restore);
    }

    #[test]
    fn runtimes_report_size_deletability_and_optional_last_use() {
        let (runtimes, warnings) = parse_runtimes(&runtimes_json()).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(runtimes.len(), 3);

        let by_build = |build: &str| {
            runtimes
                .iter()
                .find(|runtime| runtime.build.as_deref() == Some(build))
                .unwrap()
        };
        let never_used = by_build("22S99");
        assert_eq!(never_used.last_used_unix, None);
        assert_eq!(never_used.size_bytes, 4_541_193_378);
        assert!(never_used.deletable);

        let used = by_build("23T570");
        assert_eq!(used.last_used_unix, Some(1_790_605_318));

        let locked = by_build("23F77");
        assert!(!locked.deletable);
        let planned = runtime_plan(locked, &[], Some(&[]));
        assert!(!planned.cleanable);
        assert!(planned.network_restore);
        assert_eq!(planned.label.as_deref(), Some("iOS 26.5 (23F77)"));
        assert_eq!(
            planned.sizing,
            Sizing::Reported {
                bytes: 8_494_282_293,
                last_used_unix: Some(1_791_493_232)
            }
        );
    }

    #[test]
    fn plans_are_cleanable_only_when_nothing_is_running() {
        let devices = trusted_devices(parse_devices(&devices_json()).unwrap());
        assert_eq!(devices[0].state, "Shutdown");
        assert!(device_plan(&devices[0]).unwrap().cleanable);
        assert!(!device_plan(&devices[1]).unwrap().cleanable, "booted");

        let (runtimes, _) = parse_runtimes(&runtimes_json()).unwrap();
        let usable = runtimes
            .iter()
            .find(|runtime| runtime.build.as_deref() == Some("22S99"))
            .unwrap();
        assert!(runtime_plan(usable, &[], Some(&[])).cleanable);
        assert!(
            !runtime_plan(usable, &[], None).cleanable,
            "device list unavailable"
        );
        let booted = Device {
            name: "Watch".to_owned(),
            state: "Booted".to_owned(),
            runtime_key: usable.runtime_identifier.clone(),
            home: None,
        };
        assert!(!runtime_plan(usable, &[], Some(&[booted])).cleanable);
    }

    fn runtime(key: &str, version: Option<&str>, build: &str) -> Runtime {
        Runtime {
            identifier: "5C29E7B8-A172-4CC5-B491-8F4DDA3C9AB2".to_owned(),
            runtime_identifier: format!("{RUNTIME_KEY_PREFIX}{key}"),
            path: PathBuf::from(format!(
                "/Library/Developer/CoreSimulator/Images/{build}.dmg"
            )),
            build: Some(build.to_owned()),
            version: version.map(str::to_owned),
            size_bytes: 4_000_000_000,
            deletable: true,
            last_used_unix: None,
        }
    }

    fn device_on(runtime: &Runtime, state: &str, udid: &str) -> Device {
        Device {
            name: format!("Device {udid}"),
            state: state.to_owned(),
            runtime_key: runtime.runtime_identifier.clone(),
            home: Some(DeviceHome {
                udid: udid.to_owned(),
                directory: PathBuf::from(format!("/Users/example/Devices/{udid}")),
            }),
        }
    }

    const NO_APPS: fn(&Path) -> bool = |_| false;

    #[test]
    fn an_older_runtime_whose_devices_are_all_empty_is_superseded() {
        let old = runtime("watchOS-11-2", Some("11.2"), "22S99");
        let new = runtime("watchOS-26-5", Some("26.5"), "23T570");
        let runtimes = [old.clone(), new.clone()];

        assert_eq!(
            superseded(&old, &runtimes, &[], NO_APPS).as_deref(),
            Some("superseded by watchOS 26.5; no simulator device uses it")
        );
        let one = [device_on(&old, SHUTDOWN, "A")];
        assert_eq!(
            superseded(&old, &runtimes, &one, NO_APPS).as_deref(),
            Some("superseded by watchOS 26.5; its one simulator device holds no apps")
        );
        let two = [
            device_on(&old, SHUTDOWN, "A"),
            device_on(&old, SHUTDOWN, "B"),
        ];
        assert_eq!(
            superseded(&old, &runtimes, &two, NO_APPS).as_deref(),
            Some("superseded by watchOS 26.5; its 2 simulator devices hold no apps")
        );
        assert_eq!(superseded(&new, &runtimes, &two, NO_APPS), None, "newest");
        let busy_elsewhere = [device_on(&new, "Booted", "C")];
        assert!(
            superseded(&old, &runtimes, &busy_elsewhere, |_| true).is_some(),
            "devices on other runtimes do not count"
        );
    }

    #[test]
    fn a_device_that_runs_holds_apps_or_cannot_be_inspected_keeps_its_runtime() {
        let old = runtime("iOS-18-2", Some("18.2"), "22C150");
        let runtimes = [old.clone(), runtime("iOS-26-5", Some("26.5"), "23F77")];
        let holds = device_on(&old, SHUTDOWN, "HOLDS");
        let empty = device_on(&old, SHUTDOWN, "EMPTY");
        let apps_in_holds = |directory: &Path| directory.ends_with("HOLDS");

        assert_eq!(
            superseded(&old, &runtimes, &[empty, holds], apps_in_holds),
            None,
            "a device holds apps"
        );
        let booted = device_on(&old, "Booted", "EMPTY");
        assert_eq!(
            superseded(&old, &runtimes, &[booted], NO_APPS),
            None,
            "running"
        );
        let mut untrusted = device_on(&old, SHUTDOWN, "EMPTY");
        untrusted.home = None;
        assert_eq!(
            superseded(&old, &runtimes, &[untrusted], NO_APPS),
            None,
            "contents unknown"
        );
    }

    #[test]
    fn only_a_strictly_newer_runtime_of_the_same_platform_supersedes() {
        let ios_18 = runtime("iOS-18-2", Some("18.2"), "22C150");
        let watch_26 = runtime("watchOS-26-5", Some("26.5"), "23T570");
        assert_eq!(
            superseded(&ios_18, &[ios_18.clone(), watch_26], &[], NO_APPS),
            None,
            "another platform"
        );
        let rebuild = runtime("iOS-18-2", Some("18.2"), "22C151");
        assert_eq!(
            superseded(&ios_18, &[ios_18.clone(), rebuild], &[], NO_APPS),
            None,
            "same version"
        );
        let ios_9 = runtime("iOS-9-3", Some("9.3"), "13E230");
        let ios_10 = runtime("iOS-10-0", Some("10.0"), "14A345");
        assert!(
            superseded(&ios_9, &[ios_9.clone(), ios_10.clone()], &[], NO_APPS).is_some(),
            "versions compare numerically"
        );
        assert_eq!(
            superseded(&ios_10, &[ios_9.clone(), ios_10.clone()], &[], NO_APPS),
            None
        );
        let ios_9_5 = runtime("iOS-9-5", Some("9.5"), "13G36");
        assert_eq!(
            superseded(&ios_9, &[ios_9.clone(), ios_9_5, ios_10], &[], NO_APPS).as_deref(),
            Some("superseded by iOS 10.0; no simulator device uses it"),
            "the newest is chosen numerically"
        );
        let point = runtime("iOS-18-2-1", None, "22C161");
        assert_eq!(
            superseded(&ios_18, &[ios_18.clone(), point], &[], NO_APPS).as_deref(),
            Some("superseded by iOS 18.2.1; no simulator device uses it"),
            "version read from the identifier"
        );
        let newest = [
            ios_18.clone(),
            runtime("iOS-26-5", Some("26.5"), "23F77"),
            runtime("iOS-26-0", Some("26.0"), "23A339"),
        ];
        assert!(
            superseded(&ios_18, &newest, &[], NO_APPS)
                .unwrap()
                .starts_with("superseded by iOS 26.5;"),
            "names the newest"
        );
    }

    #[test]
    fn a_superseded_runtime_is_unprotected_only_with_the_device_list() {
        let old = runtime("watchOS-11-2", Some("11.2"), "22S99");
        let runtimes = [old.clone(), runtime("watchOS-26-5", Some("26.5"), "23T570")];

        let without_devices = runtime_plan(&old, &runtimes, None);
        assert_eq!(without_devices.unprotected_because, None);
        let measured = native::measure(without_devices, 7, 2_000_000_000).unwrap();
        assert!(measured.protected, "unknown use stays protected");

        let planned = runtime_plan(&old, &runtimes, Some(&[]));
        assert!(planned.cleanable);
        let measured = native::measure(planned, 7, 2_000_000_000).unwrap();
        assert_eq!(measured.age_days, None);
        assert!(!measured.protected);
        assert_eq!(
            measured.protection_note.as_deref(),
            Some("superseded by watchOS 26.5; no simulator device uses it")
        );

        let newest = runtime_plan(&runtimes[1], &runtimes, Some(&[]));
        assert!(native::measure(newest, 7, 2_000_000_000).unwrap().protected);
    }

    #[test]
    fn a_running_device_without_a_trusted_path_still_blocks_its_runtime() {
        let (runtimes, _) = parse_runtimes(&runtimes_json()).unwrap();
        let usable = runtimes
            .iter()
            .find(|runtime| runtime.build.as_deref() == Some("22S99"))
            .unwrap();
        let key = &usable.runtime_identifier;
        for entry in [
            r#"{"state":"Booted","name":"No Path","udid":"7A1C3F10-0000-4000-8000-000000000001"}"#,
            r#"{"state":"Booted","name":"Bad Path","udid":"7A1C3F10-0000-4000-8000-000000000001","dataPath":"relative/data"}"#,
            r#"{"state":"Booted"}"#,
            r#"{"name":"No State","udid":"7A1C3F10-0000-4000-8000-000000000001"}"#,
            r#""not an object""#,
        ] {
            let json = format!(r#"{{"devices":{{"{key}":[{entry}]}}}}"#);
            let devices = parse_devices(&json).unwrap();
            assert_eq!(devices.len(), 1, "{entry}");
            assert!(devices[0].home.is_none(), "{entry}");
            assert!(device_plan(&devices[0]).is_none(), "never offered: {entry}");
            assert!(
                device_using(usable, &devices).is_some(),
                "blocks the runtime: {entry}"
            );
            assert!(
                !runtime_plan(usable, &[], Some(&devices)).cleanable,
                "{entry}"
            );
        }
    }

    #[test]
    fn absence_is_established_from_the_raw_listing() {
        let id = "5C29E7B8-A172-4CC5-B491-8F4DDA3C9AB2";
        let other = "15CCF121-0865-4533-B65E-582CB34C5835";
        assert!(runtime_absent("{}", id));
        assert!(runtime_absent(
            &runtimes_json(),
            "00000000-0000-4000-8000-000000000000"
        ));
        assert!(
            !runtime_absent(&runtimes_json(), id),
            "listed and parseable"
        );
        // Present but with no size, so parse_runtimes would drop it.
        let unparseable = format!(r#"{{"{id}":{{"identifier":"{id}","deletable":true}}}}"#);
        assert!(parse_runtimes(&unparseable).unwrap().0.is_empty());
        assert!(
            !runtime_absent(&unparseable, id),
            "skipped entry still listed"
        );
        let by_field = format!(r#"{{"k":{{"identifier":"{id}"}}}}"#);
        assert!(!runtime_absent(&by_field, id));
        assert!(runtime_absent(&by_field, other));
        for broken in ["", "not json", "[]", "null"] {
            assert!(!runtime_absent(broken, id), "{broken:?}");
        }
    }

    #[test]
    fn runtimes_without_a_size_or_path_are_skipped_with_a_warning() {
        let json = r#"{
  "5C29E7B8-A172-4CC5-B491-8F4DDA3C9AB2":{"build":"22S99","deletable":true,"identifier":"5C29E7B8-A172-4CC5-B491-8F4DDA3C9AB2","path":"/example/one.dmg","runtimeIdentifier":"com.apple.CoreSimulator.SimRuntime.watchOS-11-2","version":"11.2"},
  "15CCF121-0865-4533-B65E-582CB34C5835":{"deletable":true,"identifier":"15CCF121-0865-4533-B65E-582CB34C5835","runtimeIdentifier":"x","sizeBytes":1},
  "--flag":{"deletable":true,"identifier":"--flag","path":"/example/two.dmg","runtimeIdentifier":"x","sizeBytes":1}
}"#;
        let (runtimes, warnings) = parse_runtimes(json).unwrap();
        assert!(runtimes.is_empty());
        assert_eq!(warnings.len(), 3);
    }

    #[test]
    fn a_runtime_that_is_not_deletable_unless_stated() {
        let json = r#"{"5C29E7B8-A172-4CC5-B491-8F4DDA3C9AB2":{"identifier":"5C29E7B8-A172-4CC5-B491-8F4DDA3C9AB2","path":"/example/one.dmg","runtimeIdentifier":"x","sizeBytes":5}}"#;
        let (runtimes, _) = parse_runtimes(json).unwrap();
        assert!(!runtimes[0].deletable);
    }

    #[test]
    fn malformed_simctl_output_is_an_error_not_an_empty_list() {
        assert!(parse_devices("not json").is_err());
        assert!(parse_devices("{}").is_err());
        assert!(parse_runtimes("[]").is_err());
        assert!(parse_runtimes("").is_err());
    }

    #[test]
    fn timestamps_parse_strictly() {
        assert_eq!(parse_utc_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_utc_timestamp("2000-03-01T00:00:00Z"),
            Some(951_868_800)
        );
        assert_eq!(
            parse_utc_timestamp("2024-02-29T12:30:45Z"),
            Some(1_709_209_845)
        );
        assert_eq!(
            parse_utc_timestamp("2024-02-29T12:30:45.123Z"),
            Some(1_709_209_845)
        );
        for bad in [
            "",
            "2024-02-29",
            "2024-02-29T12:30:45",
            "2024-02-29T12:30:45+01:00",
            "2024-13-01T00:00:00Z",
            "2024-00-01T00:00:00Z",
            "2024-01-01T25:00:00Z",
            "24-01-01T00:00:00Z",
            "1969-12-31T23:59:59Z",
            "2024-01-01T00:00Z",
            "2024-01-01T00:00:00.xZ",
            "garbage",
        ] {
            assert_eq!(parse_utc_timestamp(bad), None, "{bad}");
        }
    }

    #[test]
    fn identifiers_must_be_canonical_uuids() {
        assert!(valid_udid("2B5F8808-A600-48F0-9750-81CCBA294022"));
        for bad in [
            "",
            "-h",
            "../x",
            "2B5F8808-A600-48F0-9750-81CCBA29402",
            "2B5F8808A60048F0975081CCBA294022",
            "2B5F8808-A600-48F0-9750-81CCBA29402G",
        ] {
            assert!(!valid_udid(bad), "{bad}");
        }
    }
}

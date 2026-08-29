use std::path::{Path, PathBuf};

// ── Constants ─────────────────────────────────────────────────────────────────

#[cfg(feature = "udisks2")]
const UDISKS2_DEST: &str = "org.freedesktop.UDisks2";
#[cfg(feature = "udisks2")]
const UDISKS2_PATH: &str = "/org/freedesktop/UDisks2";
#[cfg(feature = "udisks2")]
const IFACE_BLOCK: &str = "org.freedesktop.UDisks2.Block";
#[cfg(feature = "udisks2")]
const IFACE_DRIVE: &str = "org.freedesktop.UDisks2.Drive";
#[cfg(feature = "udisks2")]
const IFACE_FS: &str = "org.freedesktop.UDisks2.Filesystem";
#[cfg(feature = "udisks2")]
const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(feature = "udisks2")]
const METHOD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
#[cfg(feature = "udisks2")]
const MAX_CONSECUTIVE_ERR: u32 = 5;

// ── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovableDevice {
    pub mount_point: Option<PathBuf>, // e.g. /run/media/$USER/USB; None = plugged but not mounted
    pub devnode: Option<String>,      // e.g. "/dev/sdb1"; None when no /dev/ source
    pub label: String,                // mount_point file_name(); fallback = full mount-point string
    pub is_optical: bool,             // base block dev starts with "sr" OR fstype is iso9660/udf
    pub object_path: Option<String>,  // udisks2 object path; None on poll path
}

#[cfg_attr(not(feature = "udisks2"), allow(dead_code))]
pub enum DeviceAction {
    Mount { object_path: String },
}

pub enum DeviceEvent {
    Snapshot(Vec<RemovableDevice>),
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn removable_flag(sys_root: &Path, dev_name: &str) -> Option<String> {
    let p1 = sys_root.join(format!("block/{}/removable", dev_name));
    let p2 = sys_root.join(format!("block/{}/device/removable", dev_name));
    let read_val = |p: &Path| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string());
    match read_val(&p1) {
        Some(v) => Some(v),
        None => read_val(&p2),
    }
}

pub fn parse_mounts(mounts_text: &str, sys_root: &Path) -> Vec<RemovableDevice> {
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut out: Vec<RemovableDevice> = Vec::new();

    for line in mounts_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let source = fields[0];
        let mount_point = fields[1];
        let fstype = fields[2];

        let dev_name = match source.strip_prefix("/dev/") {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };

        // Whole devices like sr0 keep their trailing digit (block/sr0 exists);
        // partitions (sdb1, nvme0n1p2) are children of block/<parent>, so strip
        // partition suffixes only when the full name does not resolve.
        let mut base = dev_name;
        let mut flag = removable_flag(sys_root, base);
        if flag.is_none() {
            let mut b = dev_name.trim_end_matches(|c: char| c.is_ascii_digit());
            if let Some(s) = b.strip_suffix('p') {
                b = s;
            }
            if !b.is_empty() {
                base = b;
                flag = removable_flag(sys_root, base);
            }
        }
        let Some(val) = flag else { continue };
        if val != "1" {
            continue;
        }

        let mp = PathBuf::from(mount_point);
        if seen.contains(&mp) {
            continue;
        }
        seen.insert(mp.clone());

        let label = mp
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| mp.to_string_lossy().into_owned());

        let is_optical = base.starts_with("sr") || fstype == "iso9660" || fstype == "udf";

        out.push(RemovableDevice {
            mount_point: Some(mp),
            devnode: Some(source.to_string()),
            label,
            is_optical,
            object_path: None,
        });
    }

    // Stable-sort by mount_point
    out.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    out
}

pub fn enumerate_removable_mounts() -> Vec<RemovableDevice> {
    let text = match std::fs::read_to_string("/proc/self/mounts") {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    parse_mounts(&text, Path::new("/sys"))
}

// ── Poll loop (fallback) ─────────────────────────────────────────────────────

fn poll_loop(ctx: eframe::egui::Context, tx: std::sync::mpsc::Sender<DeviceEvent>) {
    std::thread::spawn(move || {
        let mut last: Option<Vec<RemovableDevice>> = None;
        loop {
            let list = enumerate_removable_mounts();
            let changed = match &last {
                Some(prev) => *prev != list,
                None => true,
            };
            if changed {
                let _ = tx.send(DeviceEvent::Snapshot(list.clone()));
                ctx.request_repaint();
                last = Some(list);
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}

pub fn spawn_device_monitor(
    ctx: eframe::egui::Context,
    action_rx: std::sync::mpsc::Receiver<DeviceAction>,
) -> std::sync::mpsc::Receiver<DeviceEvent> {
    #[cfg(feature = "udisks2")]
    {
        if let Some(rx) = try_spawn_udisks2_monitor(ctx.clone(), action_rx) {
            return rx;
        }
    }
    #[cfg(not(feature = "udisks2"))]
    let _ = action_rx;

    let (tx, rx) = std::sync::mpsc::channel();
    poll_loop(ctx, tx);
    rx
}

// ── udisks2 path ────────────────────────────────────────────────────────────

#[cfg(feature = "udisks2")]
fn try_spawn_udisks2_monitor(
    ctx: eframe::egui::Context,
    action_rx: std::sync::mpsc::Receiver<DeviceAction>,
) -> Option<std::sync::mpsc::Receiver<DeviceEvent>> {
    let conn = zbus::blocking::connection::Builder::system()
        .ok()?
        .method_timeout(METHOD_TIMEOUT)
        .build()
        .ok()?;
    let om = zbus::blocking::fdo::ObjectManagerProxy::new(&conn, UDISKS2_DEST, UDISKS2_PATH).ok()?;
    if om.get_managed_objects().is_err() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || udisks2_monitor_loop(ctx, conn, action_rx, tx));
    Some(rx)
}

#[cfg(feature = "udisks2")]
fn udisks2_monitor_loop(
    ctx: eframe::egui::Context,
    conn: zbus::blocking::Connection,
    action_rx: std::sync::mpsc::Receiver<DeviceAction>,
    tx: std::sync::mpsc::Sender<DeviceEvent>,
) {
    let dirty = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

    // Signal helper threads (optimization; correctness floor is the 2s refresh)
    {
        let dirty_clone = dirty.clone();
        let conn_clone = conn.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let Ok(om) =
                zbus::blocking::fdo::ObjectManagerProxy::new(&conn_clone, UDISKS2_DEST, UDISKS2_PATH)
            else {
                return;
            };
            let Ok(mut it) = om.receive_interfaces_added() else {
                return;
            };
            while it.next().is_some() {
                dirty_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                ctx_clone.request_repaint();
            }
        });
    }
    {
        let dirty_clone = dirty.clone();
        let conn_clone = conn.clone();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let Ok(om) =
                zbus::blocking::fdo::ObjectManagerProxy::new(&conn_clone, UDISKS2_DEST, UDISKS2_PATH)
            else {
                return;
            };
            let Ok(mut it) = om.receive_interfaces_removed() else {
                return;
            };
            while it.next().is_some() {
                dirty_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                ctx_clone.request_repaint();
            }
        });
    }

    let mut last: Option<Vec<RemovableDevice>> = None;
    let mut last_refresh = std::time::Instant::now();
    let mut errs: u32 = 0;
    loop {
        while let Ok(action) = action_rx.try_recv() {
            let conn = conn.clone();
            let dirty = dirty.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                handle_mount(&conn, &action);
                dirty.store(true, std::sync::atomic::Ordering::Relaxed);
                ctx.request_repaint();
            });
        }
        let due = dirty.swap(false, std::sync::atomic::Ordering::Relaxed)
            || last_refresh.elapsed() >= REFRESH_INTERVAL;
        if due {
            match udisks2_refresh(&conn) {
                Ok(list) => {
                    errs = 0;
                    if last.as_ref() != Some(&list) {
                        let _ = tx.send(DeviceEvent::Snapshot(list.clone()));
                        ctx.request_repaint();
                    }
                    last = Some(list);
                    last_refresh = std::time::Instant::now();
                }
                Err(_) => {
                    errs += 1;
                    if errs >= MAX_CONSECUTIVE_ERR {
                        eprintln!("vibeprint: udisks2 unreachable, falling back to /proc polling");
                        break;
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    drop(action_rx);
    poll_loop(ctx, tx);
}

#[cfg(feature = "udisks2")]
fn udisks2_refresh(conn: &zbus::blocking::Connection) -> Result<Vec<RemovableDevice>, zbus::Error> {
    let om = zbus::blocking::fdo::ObjectManagerProxy::new(conn, UDISKS2_DEST, UDISKS2_PATH)?;
    let objects = om.get_managed_objects()?;
    Ok(parse_managed_objects(&objects))
}

#[cfg(feature = "udisks2")]
fn parse_managed_objects(objects: &zbus::fdo::ManagedObjects) -> Vec<RemovableDevice> {
    let block_iface =
        zbus::names::OwnedInterfaceName::try_from(IFACE_BLOCK).unwrap();
    let drive_iface =
        zbus::names::OwnedInterfaceName::try_from(IFACE_DRIVE).unwrap();
    let fs_iface = zbus::names::OwnedInterfaceName::try_from(IFACE_FS).unwrap();

    let mut out: Vec<RemovableDevice> = Vec::new();

    for (path, ifaces) in objects {
        let Some(block_props) = ifaces.get(&block_iface) else {
            continue;
        };

        // Device (ay -> Vec<u8>)
        let device_bytes: Vec<u8> = match block_props
            .get("Device")
            .and_then(|v| Vec::<u8>::try_from(v.clone()).ok())
        {
            Some(b) => b,
            None => continue,
        };
        let dev_str = String::from_utf8_lossy(&device_bytes)
            .trim_end_matches('\0')
            .to_string();
        if dev_str.is_empty() {
            continue;
        }

        // HintSystem (b, default false)
        let hint_system = block_props
            .get("HintSystem")
            .and_then(|v| bool::try_from(v.clone()).ok())
            .unwrap_or(false);
        if hint_system {
            continue;
        }

        // IdLabel (s)
        let id_label_owned: Option<String> = block_props
            .get("IdLabel")
            .and_then(|v| String::try_from(v.clone()).ok());
        let id_label = id_label_owned.as_deref().unwrap_or("");

        // Drive (o -> OwnedObjectPath string; may be absent or "/")
        let drive_path_str: Option<String> = block_props
            .get("Drive")
            .and_then(|v| zbus::zvariant::OwnedObjectPath::try_from(v.clone()).ok())
            .map(|p| p.as_str().to_string());

        // Look up drive object in same `objects` map
        let drive_props_opt: Option<&std::collections::HashMap<String, zbus::zvariant::OwnedValue>> =
            drive_path_str.as_deref().and_then(|s| {
                if s == "/" || s.is_empty() {
                    None
                } else {
                    zbus::zvariant::OwnedObjectPath::try_from(s)
                        .ok()
                        .and_then(|oop| objects.get(&oop))
                        .and_then(|m| m.get(&drive_iface))
                }
            });

        let (removable, media_removable, optical, drive_id, drive_model) =
            if let Some(dp) = drive_props_opt {
                let rem = dp
                    .get("Removable")
                    .and_then(|v| bool::try_from(v.clone()).ok())
                    .unwrap_or(false);
                let med = dp
                    .get("MediaRemovable")
                    .and_then(|v| bool::try_from(v.clone()).ok())
                    .unwrap_or(false);
                let opt = dp
                    .get("Optical")
                    .and_then(|v| bool::try_from(v.clone()).ok())
                    .unwrap_or(false);
                let id = dp
                    .get("Id")
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_default();
                let model = dp
                    .get("Model")
                    .and_then(|v| String::try_from(v.clone()).ok())
                    .unwrap_or_default();
                (rem, med, opt, id, model)
            } else {
                (false, false, false, String::new(), String::new())
            };

        // Include filter
        let keep = if drive_props_opt.is_some() {
            removable || media_removable || optical
        } else {
            true // drive absent -> keep only when !HintSystem (already checked)
        };
        if !keep {
            continue;
        }

        // Filesystem.MountPoints (aay -> Vec<Vec<u8>>)
        let mount_point: Option<PathBuf> = ifaces
            .get(&fs_iface)
            .and_then(|fs_props| fs_props.get("MountPoints"))
            .and_then(|v| Vec::<Vec<u8>>::try_from(v.clone()).ok())
            .and_then(|vecs| {
                for bytes in vecs {
                    if bytes.is_empty() {
                        continue;
                    }
                    let s = String::from_utf8_lossy(&bytes)
                        .trim_end_matches('\0')
                        .to_string();
                    if s.is_empty() {
                        continue;
                    }
                    return Some(PathBuf::from(s));
                }
                None
            });

        // label fallback
        let label = if !id_label.is_empty() {
            id_label.to_string()
        } else if !drive_id.is_empty() {
            drive_id.clone()
        } else if !drive_model.is_empty() {
            drive_model.clone()
        } else {
            "Removable Device".to_string()
        };

        let is_optical = optical;

        out.push(RemovableDevice {
            mount_point,
            devnode: Some(dev_str),
            label,
            is_optical,
            object_path: Some(path.to_string()),
        });
    }

    out.sort_by(|a, b| (a.mount_point.is_none(), &a.label).cmp(&(b.mount_point.is_none(), &b.label)));
    out
}

#[cfg(feature = "udisks2")]
fn handle_mount(conn: &zbus::blocking::Connection, action: &DeviceAction) {
    let DeviceAction::Mount { object_path } = action;
    let proxy = match zbus::blocking::Proxy::new(conn, UDISKS2_DEST, object_path.as_str(), IFACE_FS) {
        Ok(p) => p,
        Err(_) => return,
    };
    let opts: std::collections::HashMap<String, zbus::zvariant::Value> = Default::default();
    let _ = proxy.call::<_, _, String>("Mount", &opts);
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_mounts_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let sys_root = tmp.path();

        // Create block/sdb/removable = "1"
        for (dev, val) in &[("sdb", "1"), ("sda", "0"), ("sr0", "1"), ("mmcblk0", "1")] {
            let p = sys_root.join(format!("block/{}/removable", dev));
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, *val).unwrap();
        }
        // Intentionally do NOT create block/nvme0n1/removable -> should be absent

        let mounts_text = "\
/dev/sdb1 /run/media/user/USB vfat rw,nosuid,nodev 0 0
/dev/sda1 / ext4 rw,relatime 0 0
/dev/sr0 /run/media/user/MY_DVD iso9660 ro,noexec 0 0
proc /proc proc rw,nosuid,nodev,noexec 0 0
tmpfs /run/user/1000 tmpfs rw,nosuid,nodev 0 0
/dev/nvme0n1p2 /home ext4 rw,relatime 0 0
/dev/mmcblk0p1 /run/media/user/CARD vfat rw 0 0
";

        let result = parse_mounts(mounts_text, sys_root);

        // Should contain USB, MY_DVD, CARD
        assert!(
            result.iter().any(|d| d.mount_point.as_deref() == Some(Path::new("/run/media/user/USB"))
                && d.devnode.as_deref() == Some("/dev/sdb1")
                && d.label == "USB"
                && !d.is_optical
                && d.object_path.is_none()),
            "USB missing or wrong: {:?}",
            result
        );
        assert!(
            result.iter().any(|d| d.mount_point.as_deref() == Some(Path::new("/run/media/user/MY_DVD"))
                && d.devnode.as_deref() == Some("/dev/sr0")
                && d.label == "MY_DVD"
                && d.is_optical
                && d.object_path.is_none()),
            "MY_DVD missing or wrong: {:?}",
            result
        );
        assert!(
            result.iter().any(|d| d.mount_point.as_deref() == Some(Path::new("/run/media/user/CARD"))
                && d.devnode.as_deref() == Some("/dev/mmcblk0p1")
                && d.label == "CARD"
                && d.object_path.is_none()),
            "CARD missing or wrong: {:?}",
            result
        );

        // Should NOT contain /, proc, tmpfs, /home
        assert!(!result
            .iter()
            .any(|d| d.mount_point.as_deref() == Some(Path::new("/"))));
        assert!(!result
            .iter()
            .any(|d| d.mount_point.as_deref() == Some(Path::new("/proc"))));
        assert!(!result
            .iter()
            .any(|d| d.mount_point.as_deref() == Some(Path::new("/run/user/1000"))));
        assert!(!result
            .iter()
            .any(|d| d.mount_point.as_deref() == Some(Path::new("/home"))));

        // Sorted by mount_point
        let mps: Vec<PathBuf> = result
            .iter()
            .filter_map(|d| d.mount_point.clone())
            .collect();
        let mut sorted = mps.clone();
        sorted.sort();
        assert_eq!(mps, sorted, "not sorted: {:?}", mps);
        assert_eq!(result.len(), 3);
        for d in &result {
            assert!(d.object_path.is_none());
        }
    }

    #[test]
    fn parse_mounts_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let sys_root = tmp.path();
        let p = sys_root.join("block/sdb/removable");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "1").unwrap();

        let mounts_text = "\
/dev/sdb1 /run/media/user/USB vfat rw 0 0
/dev/sdb1 /run/media/user/USB vfat rw 0 0
";
        let result = parse_mounts(mounts_text, sys_root);
        assert_eq!(result.len(), 1, "dedup failed: {:?}", result);
        assert!(result[0].object_path.is_none());
        assert_eq!(
            result[0].mount_point.as_deref(),
            Some(Path::new("/run/media/user/USB"))
        );
    }

    #[test]
    fn parse_mounts_fallback_device_removable() {
        let tmp = tempfile::tempdir().unwrap();
        let sys_root = tmp.path();
        // Only device/removable exists
        let p = sys_root.join("block/sdb/device/removable");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "1").unwrap();

        let mounts_text = "/dev/sdb1 /run/media/user/USB vfat rw 0 0\n";
        let result = parse_mounts(mounts_text, sys_root);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].label, "USB");
        assert_eq!(
            result[0].mount_point.as_deref(),
            Some(Path::new("/run/media/user/USB"))
        );
        assert!(result[0].object_path.is_none());
    }

    #[test]
    fn parse_mounts_optical_by_fstype() {
        let tmp = tempfile::tempdir().unwrap();
        let sys_root = tmp.path();
        // sdb is removable but fstype udf should flag optical even though base != sr
        let p = sys_root.join("block/sdb/removable");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "1").unwrap();

        let mounts_text = "/dev/sdb1 /mnt/dvd udf ro 0 0\n";
        let result = parse_mounts(mounts_text, sys_root);
        assert_eq!(result.len(), 1);
        assert!(result[0].is_optical, "udf should be optical");
        assert_eq!(
            result[0].mount_point.as_deref(),
            Some(Path::new("/mnt/dvd"))
        );
        assert!(result[0].object_path.is_none());
    }

    #[test]
    fn parse_mounts_removable_trim() {
        let tmp = tempfile::tempdir().unwrap();
        let sys_root = tmp.path();
        let p = sys_root.join("block/sdb/removable");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "1\n").unwrap(); // with newline, trimmed => still "1"

        let mounts_text = "/dev/sdb1 /run/media/user/USB vfat rw 0 0\n";
        let result = parse_mounts(mounts_text, sys_root);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].mount_point.as_deref(),
            Some(Path::new("/run/media/user/USB"))
        );
        assert!(result[0].object_path.is_none());
    }

    #[test]
    fn parse_mounts_whole_device_with_digit_non_removable() {
        let tmp = tempfile::tempdir().unwrap();
        let sys_root = tmp.path();
        let p = sys_root.join("block/zram0/removable");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, "0").unwrap();

        let mounts_text = "/dev/zram0 /var/tmp zram rw 0 0\n";
        let result = parse_mounts(mounts_text, sys_root);
        assert!(
            !result
                .iter()
                .any(|d| d.mount_point.as_deref() == Some(Path::new("/var/tmp"))),
            "zram0 should not be included: {:?}",
            result
        );
        assert_eq!(result.len(), 0);
    }

    // ── udisks2 parse_managed_objects tests ─────────────────────────────

    #[cfg(all(test, feature = "udisks2"))]
    mod udisks2_tests {
        use super::super::*;
        use std::collections::HashMap;
        use std::path::Path;
        use zbus::names::OwnedInterfaceName;
        use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

        fn mo_path(s: &str) -> OwnedObjectPath {
            OwnedObjectPath::try_from(s).unwrap()
        }
        fn iface(s: &str) -> OwnedInterfaceName {
            OwnedInterfaceName::try_from(s).unwrap()
        }
        fn ov_str(s: &str) -> OwnedValue {
            Value::new(s).try_into().unwrap()
        }
        #[allow(dead_code)]
        fn ov_string(s: String) -> OwnedValue {
            Value::new(s).try_into().unwrap()
        }
        fn ov_bytes(v: Vec<u8>) -> OwnedValue {
            Value::new(v).try_into().unwrap()
        }
        fn ov_bytes_vec(v: Vec<Vec<u8>>) -> OwnedValue {
            Value::new(v).try_into().unwrap()
        }
        fn ov_bool(b: bool) -> OwnedValue {
            OwnedValue::from(b)
        }
        fn ov_oop(s: &str) -> OwnedValue {
            Value::new(mo_path(s)).try_into().unwrap()
        }

        fn make_mounted_usb() -> zbus::fdo::ManagedObjects {
            let mut objects: zbus::fdo::ManagedObjects = HashMap::new();

            // block object
            let mut block_props: HashMap<String, OwnedValue> = HashMap::new();
            block_props.insert("Device".to_string(), ov_bytes(b"/dev/sdb1".to_vec()));
            block_props.insert(
                "Drive".to_string(),
                ov_oop("/org/freedesktop/UDisks2/drives/drive1"),
            );
            block_props.insert("HintSystem".to_string(), ov_bool(false));
            block_props.insert("IdLabel".to_string(), ov_str("USB"));

            let mut fs_props: HashMap<String, OwnedValue> = HashMap::new();
            fs_props.insert(
                "MountPoints".to_string(),
                ov_bytes_vec(vec![b"/run/media/user/USB".to_vec()]),
            );

            let mut ifaces: HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>> =
                HashMap::new();
            ifaces.insert(iface(IFACE_BLOCK), block_props);
            ifaces.insert(iface(IFACE_FS), fs_props);

            objects.insert(
                mo_path("/org/freedesktop/UDisks2/block_devices/sdb1"),
                ifaces,
            );

            // drive object
            let mut drive_props: HashMap<String, OwnedValue> = HashMap::new();
            drive_props.insert("Removable".to_string(), ov_bool(true));
            drive_props.insert("MediaRemovable".to_string(), ov_bool(true));
            drive_props.insert("Optical".to_string(), ov_bool(false));
            drive_props.insert("Id".to_string(), ov_str("USB_Drive"));
            drive_props.insert("Model".to_string(), ov_str("USB Model"));

            let mut drive_ifaces: HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>> =
                HashMap::new();
            drive_ifaces.insert(iface(IFACE_DRIVE), drive_props);

            objects.insert(mo_path("/org/freedesktop/UDisks2/drives/drive1"), drive_ifaces);

            objects
        }

        #[test]
        fn udisks2_mounted_usb() {
            let objects = make_mounted_usb();
            let result = parse_managed_objects(&objects);
            assert_eq!(result.len(), 1, "expected 1 device: {:?}", result);
            let d = &result[0];
            assert_eq!(d.mount_point.as_deref(), Some(Path::new("/run/media/user/USB")));
            assert_eq!(d.label, "USB");
            assert!(!d.is_optical);
            assert_eq!(
                d.object_path.as_deref(),
                Some("/org/freedesktop/UDisks2/block_devices/sdb1")
            );
            assert_eq!(d.devnode.as_deref(), Some("/dev/sdb1"));
        }

        #[test]
        fn udisks2_unmounted() {
            let mut objects = make_mounted_usb();
            // Make MountPoints empty -> unmounted
            let block_path = mo_path("/org/freedesktop/UDisks2/block_devices/sdb1");
            if let Some(ifaces) = objects.get_mut(&block_path) {
                if let Some(fs) = ifaces.get_mut(&iface(IFACE_FS)) {
                    fs.insert("MountPoints".to_string(), ov_bytes_vec(vec![]));
                }
            }
            let result = parse_managed_objects(&objects);
            assert_eq!(result.len(), 1, "expected 1 device even when unmounted: {:?}", result);
            assert_eq!(result[0].mount_point, None);
        }

        #[test]
        fn udisks2_optical() {
            let mut objects = make_mounted_usb();
            // Set Optical true
            let drive_path = mo_path("/org/freedesktop/UDisks2/drives/drive1");
            if let Some(ifaces) = objects.get_mut(&drive_path) {
                if let Some(drive) = ifaces.get_mut(&iface(IFACE_DRIVE)) {
                    drive.insert("Optical".to_string(), ov_bool(true));
                }
            }
            let result = parse_managed_objects(&objects);
            assert_eq!(result.len(), 1);
            assert!(result[0].is_optical, "expected optical true: {:?}", result[0]);
        }

        #[test]
        fn udisks2_internal_excluded() {
            let mut objects = make_mounted_usb();
            let drive_path = mo_path("/org/freedesktop/UDisks2/drives/drive1");
            if let Some(ifaces) = objects.get_mut(&drive_path) {
                if let Some(drive) = ifaces.get_mut(&iface(IFACE_DRIVE)) {
                    drive.insert("Removable".to_string(), ov_bool(false));
                    drive.insert("MediaRemovable".to_string(), ov_bool(false));
                    drive.insert("Optical".to_string(), ov_bool(false));
                }
            }
            let result = parse_managed_objects(&objects);
            assert_eq!(result.len(), 0, "internal drive should be excluded: {:?}", result);
        }

        #[test]
        fn udisks2_hint_system_excluded() {
            let mut objects = make_mounted_usb();
            let block_path = mo_path("/org/freedesktop/UDisks2/block_devices/sdb1");
            if let Some(ifaces) = objects.get_mut(&block_path) {
                if let Some(block) = ifaces.get_mut(&iface(IFACE_BLOCK)) {
                    block.insert("HintSystem".to_string(), ov_bool(true));
                }
            }
            let result = parse_managed_objects(&objects);
            assert_eq!(result.len(), 0, "HintSystem true should be excluded: {:?}", result);
        }

        #[test]
        fn udisks2_ordering_and_no_drive() {
            // Build objects with 3 devices: one mounted, two unmounted, plus a block with no Drive but HintSystem false
            let mut objects: zbus::fdo::ManagedObjects = HashMap::new();

            // Drive 1 (for mounted device)
            let mut d1_props: HashMap<String, OwnedValue> = HashMap::new();
            d1_props.insert("Removable".to_string(), ov_bool(true));
            d1_props.insert("MediaRemovable".to_string(), ov_bool(true));
            d1_props.insert("Optical".to_string(), ov_bool(false));
            d1_props.insert("Id".to_string(), ov_str("Drive1"));
            d1_props.insert("Model".to_string(), ov_str("Model1"));
            let mut d1_ifaces: HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>> = HashMap::new();
            d1_ifaces.insert(iface(IFACE_DRIVE), d1_props);
            objects.insert(mo_path("/org/freedesktop/UDisks2/drives/drive1"), d1_ifaces);

            // Drive 2 (for unmounted Beta/Alpha - reuse same drive obj for simplicity but need two drives with diff labels? Actually labels from IdLabel on block, so drive not needed for label)
            let mut d2_props: HashMap<String, OwnedValue> = HashMap::new();
            d2_props.insert("Removable".to_string(), ov_bool(true));
            d2_props.insert("MediaRemovable".to_string(), ov_bool(false));
            d2_props.insert("Optical".to_string(), ov_bool(false));
            d2_props.insert("Id".to_string(), ov_str("Drive2"));
            d2_props.insert("Model".to_string(), ov_str("Model2"));
            let mut d2_ifaces: HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>> = HashMap::new();
            d2_ifaces.insert(iface(IFACE_DRIVE), d2_props);
            objects.insert(mo_path("/org/freedesktop/UDisks2/drives/drive2"), d2_ifaces);

            // Mounted device: sdb1 Beta? Let's give label "MountedUSB" with mountpoint
            let mut b1: HashMap<String, OwnedValue> = HashMap::new();
            b1.insert("Device".to_string(), ov_bytes(b"/dev/sdb1".to_vec()));
            b1.insert("Drive".to_string(), ov_oop("/org/freedesktop/UDisks2/drives/drive1"));
            b1.insert("HintSystem".to_string(), ov_bool(false));
            b1.insert("IdLabel".to_string(), ov_str("MountedUSB"));
            let mut fs1: HashMap<String, OwnedValue> = HashMap::new();
            fs1.insert("MountPoints".to_string(), ov_bytes_vec(vec![b"/run/media/user/MountedUSB".to_vec()]));
            let mut if1: HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>> = HashMap::new();
            if1.insert(iface(IFACE_BLOCK), b1);
            if1.insert(iface(IFACE_FS), fs1);
            objects.insert(mo_path("/org/freedesktop/UDisks2/block_devices/sdb1"), if1);

            // Unmounted Beta
            let mut b2: HashMap<String, OwnedValue> = HashMap::new();
            b2.insert("Device".to_string(), ov_bytes(b"/dev/sdc1".to_vec()));
            b2.insert("Drive".to_string(), ov_oop("/org/freedesktop/UDisks2/drives/drive2"));
            b2.insert("HintSystem".to_string(), ov_bool(false));
            b2.insert("IdLabel".to_string(), ov_str("Beta"));
            let mut fs2: HashMap<String, OwnedValue> = HashMap::new();
            fs2.insert("MountPoints".to_string(), ov_bytes_vec(vec![]));
            let mut if2: HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>> = HashMap::new();
            if2.insert(iface(IFACE_BLOCK), b2);
            if2.insert(iface(IFACE_FS), fs2);
            objects.insert(mo_path("/org/freedesktop/UDisks2/block_devices/sdc1"), if2);

            // Unmounted Alpha
            let mut b3: HashMap<String, OwnedValue> = HashMap::new();
            b3.insert("Device".to_string(), ov_bytes(b"/dev/sdd1".to_vec()));
            b3.insert("Drive".to_string(), ov_oop("/org/freedesktop/UDisks2/drives/drive2"));
            b3.insert("HintSystem".to_string(), ov_bool(false));
            b3.insert("IdLabel".to_string(), ov_str("Alpha"));
            let mut fs3: HashMap<String, OwnedValue> = HashMap::new();
            fs3.insert("MountPoints".to_string(), ov_bytes_vec(vec![]));
            let mut if3: HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>> = HashMap::new();
            if3.insert(iface(IFACE_BLOCK), b3);
            if3.insert(iface(IFACE_FS), fs3);
            objects.insert(mo_path("/org/freedesktop/UDisks2/block_devices/sdd1"), if3);

            // Block with no Drive but HintSystem=false -> should be included
            let mut b4: HashMap<String, OwnedValue> = HashMap::new();
            b4.insert("Device".to_string(), ov_bytes(b"/dev/sde1".to_vec()));
            b4.insert("HintSystem".to_string(), ov_bool(false));
            b4.insert("IdLabel".to_string(), ov_str("NoDrive"));
            let mut fs4: HashMap<String, OwnedValue> = HashMap::new();
            fs4.insert("MountPoints".to_string(), ov_bytes_vec(vec![]));
            let mut if4: HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>> = HashMap::new();
            if4.insert(iface(IFACE_BLOCK), b4);
            if4.insert(iface(IFACE_FS), fs4);
            objects.insert(mo_path("/org/freedesktop/UDisks2/block_devices/sde1"), if4);

            let result = parse_managed_objects(&objects);
            assert_eq!(result.len(), 4, "expected 4 devices: {:?}", result);
            // First should be mounted
            assert!(result[0].mount_point.is_some(), "first should be mounted: {:?}", result[0]);
            assert_eq!(result[0].label, "MountedUSB");
            // Remaining unmounted sorted by label: Alpha, Beta, NoDrive
            assert_eq!(result[1].label, "Alpha");
            assert_eq!(result[2].label, "Beta");
            assert_eq!(result[3].label, "NoDrive");
            for d in &result[1..] {
                assert!(d.mount_point.is_none(), "unmounted should have None: {:?}", d);
            }
        }
    }
}

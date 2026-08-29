use std::path::{Path, PathBuf};

// ── Types ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovableDevice {
    pub mount_point: PathBuf,    // e.g. /run/media/$USER/USB
    pub devnode: Option<String>, // e.g. "/dev/sdb1"; None when no /dev/ source
    pub label: String,           // mount_point file_name(); fallback = full mount-point string
    pub is_optical: bool,        // base block dev starts with "sr" OR fstype is iso9660/udf
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
            mount_point: mp,
            devnode: Some(source.to_string()),
            label,
            is_optical,
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

pub fn spawn_device_monitor(ctx: eframe::egui::Context) -> std::sync::mpsc::Receiver<DeviceEvent> {
    let (tx, rx) = std::sync::mpsc::channel();
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
    rx
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
            result.iter().any(|d| d.mount_point == PathBuf::from("/run/media/user/USB")
                && d.devnode.as_deref() == Some("/dev/sdb1")
                && d.label == "USB"
                && !d.is_optical),
            "USB missing or wrong: {:?}",
            result
        );
        assert!(
            result.iter().any(|d| d.mount_point == PathBuf::from("/run/media/user/MY_DVD")
                && d.devnode.as_deref() == Some("/dev/sr0")
                && d.label == "MY_DVD"
                && d.is_optical),
            "MY_DVD missing or wrong: {:?}",
            result
        );
        assert!(
            result.iter().any(|d| d.mount_point == PathBuf::from("/run/media/user/CARD")
                && d.devnode.as_deref() == Some("/dev/mmcblk0p1")
                && d.label == "CARD"),
            "CARD missing or wrong: {:?}",
            result
        );

        // Should NOT contain /, proc, tmpfs, /home
        assert!(!result.iter().any(|d| d.mount_point == PathBuf::from("/")));
        assert!(!result.iter().any(|d| d.mount_point == PathBuf::from("/proc")));
        assert!(!result.iter().any(|d| d.mount_point == PathBuf::from("/run/user/1000")));
        assert!(!result.iter().any(|d| d.mount_point == PathBuf::from("/home")));

        // Sorted by mount_point
        let mps: Vec<PathBuf> = result.iter().map(|d| d.mount_point.clone()).collect();
        let mut sorted = mps.clone();
        sorted.sort();
        assert_eq!(mps, sorted, "not sorted: {:?}", mps);
        assert_eq!(result.len(), 3);
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
            !result.iter().any(|d| d.mount_point == PathBuf::from("/var/tmp")),
            "zram0 should not be included: {:?}",
            result
        );
        assert_eq!(result.len(), 0);
    }
}

use std::{
    fs::File,
    io::Read,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use xffs_tools::{DEMO_UUID, Scenario, create_image, inspect};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "xffs-tools-{}-{}.img",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
#[test]
fn reproducible_scenarios_and_forensic_labels() {
    for scenario in [
        Scenario::Clean,
        Scenario::Committed,
        Scenario::PartialCheckpoint,
    ] {
        let a = Temp::new();
        let b = Temp::new();
        for p in [&a.0, &b.0] {
            create_image(p, 64 * 1024 * 1024, DEMO_UUID, None, Some(scenario)).unwrap();
        }
        let mut a_file = File::open(&a.0).unwrap();
        let mut b_file = File::open(&b.0).unwrap();
        let mut x = [0; 65536];
        let mut y = [0; 65536];
        loop {
            let n = a_file.read(&mut x).unwrap();
            let m = b_file.read(&mut y).unwrap();
            assert_eq!(n, m);
            assert_eq!(x, y);
            if n == 0 {
                break;
            }
        }
        let text = inspect(&a.0).unwrap();
        assert!(text.contains("RAW HOME"));
        assert!(text.contains("extent_count: 5"));
        assert!(text.contains("4294971392"));
        if scenario == Scenario::Committed {
            assert!(text.contains("Before.txt"));
            assert!(text.contains("JOURNAL PAYLOAD descriptor 0"));
        } else {
            assert!(text.contains("Recovered.txt"));
        }
    }
}
#[test]
fn refuses_existing_and_preflights_geometry() {
    let p = Temp::new();
    std::fs::write(&p.0, b"untouched").unwrap();
    assert!(create_image(&p.0, 16 * 1024 * 1024, DEMO_UUID, None, None).is_err());
    assert_eq!(std::fs::read(&p.0).unwrap(), b"untouched");
    let p = Temp::new();
    assert!(create_image(&p.0, 4096, DEMO_UUID, None, None).is_err());
    assert!(!p.0.exists());
    create_image(&p.0, 16 * 1024 * 1024, DEMO_UUID, None, None).unwrap();
    assert!(inspect(&p.0).unwrap().contains("data_start: 534"));
}

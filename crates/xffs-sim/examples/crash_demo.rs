use xffs_core::{BlockDevice, DeviceError};
use xffs_sim::{Fragment, SimDevice};

fn show(label: &str, device: &SimDevice) {
    println!(
        "{label}: {:?}",
        String::from_utf8_lossy(device.durable_bytes())
    );
    for event in device.trace() {
        println!("  {event:?}");
    }
}

fn main() -> Result<(), DeviceError> {
    let mut device = SimDevice::new(4)?;
    device.write_at(0, b"lost")?;
    device.crash_and_restart(&[])?;
    assert_eq!(device.durable_bytes(), &[0; 4]);
    show("Unflushed write disappears", &device);

    device.clear_trace();
    device.write_at(0, b"safe")?;
    device.flush()?;
    device.write_at(0, b"oops")?;
    device.crash_and_restart(&[])?;
    assert_eq!(device.durable_bytes(), b"safe");
    show("Flushed write survives later unflushed overwrite", &device);

    device.clear_trace();
    device.write_at(0, b"TORN")?;
    let id = device.pending_writes()[0].id;
    device.crash_and_restart(&[Fragment {
        write_id: id,
        range: 0..2,
    }])?;
    assert_eq!(device.durable_bytes(), b"TOfe");
    show("Torn write: only first two bytes persist", &device);

    device.clear_trace();
    device.write_at(0, b"AAAA")?;
    device.write_at(2, b"BB")?;
    assert_eq!(device.visible_bytes(), b"AABB");
    let first = device.pending_writes()[0].id;
    let second = device.pending_writes()[1].id;
    device.crash_and_restart(&[
        Fragment {
            write_id: second,
            range: 0..2,
        },
        Fragment {
            write_id: first,
            range: 0..4,
        },
    ])?;
    assert_eq!(device.durable_bytes(), b"AAAA");
    show("Reordered persistence: second write, then first", &device);
    Ok(())
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! Reloads the FX2's RAM firmware on a running board without a power cycle.
//!
//! The EZ-USB core keeps handling the 0xA0 load request after ReNumeration,
//! as long as the 8051 is held in reset (EZ-USB TRM 2.3), so the usual
//! CPUCS=1, image, CPUCS=0 sequence works on an OpenVRX board too. The chip
//! then disconnects and enumerates again.
//!
//! Usage: `fx2_reload [IMAGE.ihx]` (default: the image embedded in the host).

use openvrx_hw::fx2::{firmware, list_fx2, Fx2Kind, Fx2Requests};
use std::path::Path;
use std::time::{Duration, Instant};

fn main() {
    let path = std::env::args().nth(1);
    let fw = match firmware::resolve(path.as_deref().map(Path::new)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("image: {e}");
            std::process::exit(1);
        }
    };
    let Some(found) = list_fx2().ok().and_then(|d| d.into_iter().next()) else {
        eprintln!("no FX2 on USB");
        std::process::exit(1);
    };
    println!("reloading {} ({:?})", found.describe(), found.kind);
    match found.open() {
        Ok(dev) => {
            if let Err(e) = dev.control().upload_firmware(&fw) {
                eprintln!("upload: {e}");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("open: {e}");
            std::process::exit(1);
        }
    }
    // Wait for the board to leave and come back as an OpenVRX device.
    std::thread::sleep(Duration::from_millis(600));
    let t0 = Instant::now();
    loop {
        let back = list_fx2()
            .ok()
            .and_then(|d| d.into_iter().find(|d| d.kind == Fx2Kind::Openvrx));
        if let Some(f) = back {
            match f.open().and_then(|d| d.control().get_info()) {
                Ok(info) => {
                    println!(
                        "back: {} fw {}.{}.{}",
                        f.describe(),
                        info.fw_major,
                        info.fw_minor,
                        info.fw_patch
                    );
                    return;
                }
                Err(_) if t0.elapsed() < Duration::from_secs(6) => {}
                Err(e) => {
                    eprintln!("re-enumerated but not answering: {e}");
                    std::process::exit(1);
                }
            }
        } else if t0.elapsed() > Duration::from_secs(6) {
            eprintln!("the board did not come back within 6 s");
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

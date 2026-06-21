// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Smoke test for Earlgrey SPI Host driver.
//!
//! Initializes the SPI Host, reads JEDEC ID of the external Flash,
//! and verifies it matches Macronix MX25U51245G.

#![no_std]
#![no_main]

use earlgrey_spi_host::SpiHost;
use embedded_hal::spi::SpiDevice;
use pw_status::{Error, Result};
use userspace::entry;

fn run_test() -> Result<()> {
    // 1. Initialize SPI Host (SPI_HOST0).
    // SAFETY: We have exclusive access to SPI_HOST0 in this test process.
    let mut spi = unsafe { SpiHost::new_spi0() };
    spi.init().map_err(|_| {
        pw_log::error!("SPI init failed");
        Error::Internal
    })?;

    // 2. Read JEDEC ID (Opcode 0x9F).
    let tx_buf = [0x9F];
    let mut rx_buf = [0u8; 3];

    let mut ops = [
        embedded_hal::spi::Operation::Write(&tx_buf),
        embedded_hal::spi::Operation::Read(&mut rx_buf),
    ];

    spi.transaction(&mut ops).map_err(|_| {
        pw_log::error!("SPI transaction failed");
        Error::Internal
    })?;

    pw_log::info!(
        "JEDEC ID read: {:02x} {:02x} {:02x}",
        rx_buf[0],
        rx_buf[1],
        rx_buf[2]
    );

    // 3. Verify JEDEC ID for Macronix MX25U51245G.
    // Manufacturer ID: 0xC2 (Macronix)
    // Device ID: [0x25, 0x3A] (MX25U51245G)
    if rx_buf[0] != 0xC2 {
        pw_log::error!(
            "Unexpected Manufacturer ID: {:02x} (expected 0xC2)",
            rx_buf[0]
        );
        return Err(Error::FailedPrecondition);
    }

    if rx_buf[1] != 0x25 || rx_buf[2] != 0x3A {
        pw_log::error!(
            "Unexpected Device ID: [{:02x}, {:02x}] (expected [0x25, 0x3A])",
            rx_buf[1],
            rx_buf[2]
        );
        return Err(Error::FailedPrecondition);
    }

    pw_log::info!("JEDEC ID verified successfully!");
    Ok(())
}

#[entry]
fn entry() -> Result<()> {
    pw_log::info!("🔄 RUNNING SPI Host Smoke Test");
    let ret = run_test();

    if ret.is_err() {
        pw_log::error!("❌ FAIL");
    } else {
        pw_log::info!("✅ PASS");
    }

    ret
}

util_panic::make_panic_handler!();

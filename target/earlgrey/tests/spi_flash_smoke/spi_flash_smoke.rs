// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! Smoke test for generic SPI Flash driver on Earlgrey.
//!
//! Initializes SPI Host, SpiFlash driver, wraps them in BlockingFlash,
//! and performs Erase -> Program -> Read -> Verify cycle.

#![no_std]
#![no_main]

use embedded_hal::spi::SpiDevice;
use hal_flash::{BlockingFlash, Flash};
use hal_flash_driver::{FlashAddress, FlashDriver};
use pw_status::{Error, Result};
use spi_flash::SpiFlash;
use spi_host::SpiHost0;
use userspace::entry;
use userspace::time::{Clock, Duration, SystemClock, sleep_until};
use util_types::Blocking;

// Polling-based Blocking implementation for the test.
struct SpiFlashBlocking<S: SpiDevice> {
    driver: Option<*mut SpiFlash<S>>,
}

impl<S: SpiDevice> SpiFlashBlocking<S> {
    fn new() -> Self {
        Self { driver: None }
    }
    fn set_driver(&mut self, driver: *mut SpiFlash<S>) {
        self.driver = Some(driver);
    }
}

impl<S: SpiDevice> Blocking for SpiFlashBlocking<S> {
    fn wait_for_notification(&self) {
        let Some(driver_ptr) = self.driver else {
            return;
        };
        // SAFETY:
        // 1. Liveness & Pinning: The pointer points to `flash.driver` which is owned by
        //    `flash` on the stack of `run_test`. `flash` is guaranteed not to be moved
        //    or dropped during the execution of this test, preventing dangling pointers.
        // 2. Exclusive Access: This test runs in a single-threaded context. No other thread
        //    or interrupt handler accesses the SPI Flash controller concurrently.
        // 3. No Aliasing: The active borrow of `flash.driver` in `BlockingFlash` operations
        //    (e.g., `start_program`) ends before `wait_for_notification` is called, ensuring
        //    this raw pointer access does not overlap with any safe mutable/immutable borrows.
        unsafe {
            let mut count: u32 = 0;
            while (*driver_ptr).is_busy() {
                count = count.wrapping_add(1);
                if count % 1000 == 0 {
                    let status_val = (*driver_ptr).read_status().unwrap_or(0xff);
                    pw_log::info!("Still busy... count={}, status=0x{:02x}", count, status_val);
                }
                // Sleep for 1 millisecond between WIP status checks to reduce CPU load
                let _ = sleep_until(SystemClock::now() + Duration::from_millis(1));
            }
        }
    }
}

fn run_test() -> Result<()> {
    let spi_host0 = unsafe { SpiHost0::new() };
    let mut spi_host = earlgrey_spi_host::SpiHost::new(spi_host0);
    spi_host.init().map_err(|_| Error::Internal)?;

    // 2. Create SpiFlash driver.
    let mut spi_flash = SpiFlash::new(spi_host);
    spi_flash.init().map_err(|_| Error::Internal)?;

    // 3. Wrap in BlockingFlash.
    let blocking = SpiFlashBlocking::new();
    let mut flash = BlockingFlash {
        driver: spi_flash,
        blocking,
    };
    // Set the self-reference pointer after the driver has been moved into flash.
    let driver_ptr = &mut flash.driver as *mut _;
    flash.blocking.set_driver(driver_ptr);

    // 4. Print geometry.
    let (size, page_size, erase_bitmap) = flash.geometry().map_err(|_| Error::Internal)?;
    pw_log::info!(
        "Flash geometry: size={} bytes, page_size={} bytes, erase_bitmap=0x{:x}",
        size,
        page_size.get(),
        erase_bitmap
    );

    // 5. Test Erase -> Program -> Read -> Verify
    // We use address 0x00100000 (1MB offset) which is aligned to 64KB block boundary.
    let test_addr = FlashAddress::new(0x0010_0000);

    // Erase 4KB sector.
    let erase_size = util_types::PowerOf2Usize::new(4096).unwrap();
    pw_log::info!("Erasing 4KB at offset {}...", test_addr);
    flash
        .erase(test_addr, erase_size)
        .map_err(|_| Error::Internal)?;
    pw_log::info!("Erase complete.");

    // Read back and verify it is erased (all bytes 0xFF).
    let mut read_buf = [0u8; 256];
    flash
        .read(test_addr, &mut read_buf)
        .map_err(|_| Error::Internal)?;
    for (i, &b) in read_buf.iter().enumerate() {
        if b != 0xFF {
            pw_log::error!(
                "Erase verification failed at offset +{}: expected 0xFF, got 0x{:02x}",
                i,
                b
            );
            return Err(Error::FailedPrecondition);
        }
    }
    pw_log::info!("Erase verified (all 0xFF).");

    // Program a page (256 bytes).
    let mut write_buf = [0u8; 256];
    for (i, b) in write_buf.iter_mut().enumerate() {
        *b = i as u8;
    }
    pw_log::info!("Programming 256 bytes at offset {}...", test_addr);
    flash
        .program(test_addr, &write_buf)
        .map_err(|_| Error::Internal)?;
    pw_log::info!("Program complete.");

    // Read back and verify program.
    read_buf.fill(0);
    flash
        .read(test_addr, &mut read_buf)
        .map_err(|_| Error::Internal)?;
    for (i, (&w, &r)) in write_buf.iter().zip(read_buf.iter()).enumerate() {
        if w != r {
            pw_log::error!(
                "Verification failed at offset +{}: wrote 0x{:02x}, read 0x{:02x}",
                i,
                w,
                r
            );
            return Err(Error::FailedPrecondition);
        }
    }
    pw_log::info!("Program verified successfully!");

    Ok(())
}

#[entry]
fn entry() -> Result<()> {
    pw_log::info!("🔄 RUNNING SPI Flash Smoke Test");
    let ret = run_test();

    if ret.is_err() {
        pw_log::error!("❌ FAIL");
    } else {
        pw_log::info!("✅ PASS");
    }

    ret
}

util_panic::make_panic_handler!();

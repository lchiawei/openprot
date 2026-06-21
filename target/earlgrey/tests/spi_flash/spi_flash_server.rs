// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! SPI Flash IPC server for integration testing.

#![no_std]
#![no_main]

use embedded_hal::spi::SpiDevice;
use hal_flash::BlockingFlash;
use hal_flash_driver::FlashDriver;
use pw_status::Error;
use services_flash_server::FlashIpcServer;
use spi_flash::SpiFlash;
use spi_flash_server_codegen::handle;
use userspace::entry;
use userspace::syscall;
use userspace::time::Instant;
use util_error::ErrorCode;
use util_ipc::IpcHandle;
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
        if let Some(driver_ptr) = self.driver {
            // SAFETY:
            // 1. Liveness & Pinning: The pointer points to `flash.driver` which is owned by
            //    `flash` on the stack of `run_test` (or `spi_flash_server`). `flash` is guaranteed
            //    not to be moved or dropped during the execution of this server, preventing dangling pointers.
            // 2. Exclusive Access: This server runs in a single-threaded process context. No other thread
            //    or interrupt handler accesses the SPI Flash controller concurrently.
            // 3. No Aliasing: The active borrow of `flash.driver` in `BlockingFlash` operations
            //    (e.g., `start_program`) ends before `wait_for_notification` is called, ensuring
            //    this raw pointer access does not overlap with any safe mutable/immutable borrows.
            unsafe {
                let mut count: u32 = 0;
                while (*driver_ptr).is_busy() {
                    count = count.wrapping_add(1);
                    if count % 1000 == 0 {
                        for _ in 0..1000 {
                            core::hint::spin_loop();
                        }
                    }
                }
            }
        }
    }
}

fn spi_flash_server() -> Result<(), ErrorCode> {
    pw_log::info!("spi_flash_server: initializing SPI Host");
    let mut spi_host = unsafe { earlgrey_spi_host::SpiHost::new_spi0() };
    spi_host
        .init()
        .map_err(|_| ErrorCode::kernel_error(pw_status::Error::Internal))?;

    pw_log::info!("spi_flash_server: initializing SpiFlash driver via SFDP");
    let mut spi_flash = SpiFlash::new(spi_host);
    spi_flash.init()?;

    let mut flash = BlockingFlash {
        driver: spi_flash,
        blocking: SpiFlashBlocking::new(),
    };

    let driver_ptr = &mut flash.driver as *mut _;
    flash.blocking.set_driver(driver_ptr);

    let mut flash_server = FlashIpcServer::new(&mut flash);
    let mut buf = [0u8; 2064];
    let ipc = IpcHandle::new(handle::SPI_FLASH_SERVICE);

    pw_log::info!("spi_flash_server: entering IPC loop");
    loop {
        syscall::object_wait(
            handle::SPI_FLASH_SERVICE,
            syscall::Signals::READABLE,
            Instant::MAX,
        )
        .map_err(ErrorCode::kernel_error)?;
        flash_server.handle_one(&ipc, &mut buf)?;
    }
}

#[entry]
fn entry() -> Result<(), Error> {
    pw_log::info!("🔄 SPI FLASH SERVER START");
    let ret = spi_flash_server();

    let ret = match ret {
        Ok(()) => {
            pw_log::info!("✅ SPI FLASH SERVER PASS");
            Ok(())
        }
        Err(e) => {
            pw_log::error!("❌ SPI FLASH SERVER FAIL: {:08x}", u32::from(e) as u32);
            Err(Error::Unknown)
        }
    };
    ret
}

util_panic::make_panic_handler!();

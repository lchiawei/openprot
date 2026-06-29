// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_std]
#![no_main]

use embedded_hal::spi::SpiDevice;
use combined_flash_server_codegen::{handle, signals};
use eflash_driver::{EmbeddedFlash, Permission};
use hal_flash::BlockingFlash;
use earlgrey_util::EarlgreyFlashAddress;
use hal_flash_driver::{FlashAddress, FlashDriver};
use pw_status::Error;
use spi_flash::SpiFlash;
use spi_host::SpiHost0;
use userspace::time::{Clock, Duration, Instant, SystemClock, sleep_until};
use userspace::{entry, syscall};
use services_flash_server::FlashIpcServer;
use util_error::{ErrorCode, KERNEL_ERROR_INTERNAL};
use util_ipc::IpcHandle;
use util_types::Blocking;

// 1. EFlash Interrupt Blocker
struct FlashCtrlInterrupt;

impl Blocking for FlashCtrlInterrupt {
    fn wait_for_notification(&self) {
        loop {
            if let Ok(w) = syscall::object_wait(
                handle::FLASH_INTERRUPTS,
                signals::FLASH_CTRL_OP_DONE,
                Instant::MAX,
            ) {
                if w.pending_signals.contains(signals::FLASH_CTRL_OP_DONE) {
                    break;
                }
            }
        }
        let _ = syscall::interrupt_ack(handle::FLASH_INTERRUPTS, signals::FLASH_CTRL_OP_DONE);
    }
}

// 2. SPI Flash Polling Blocker
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
        // SAFETY: The caller ensures that the SpiFlash instance pointed to by driver_ptr
        // is kept at a stable stack memory address (passed by mutable reference to the server)
        // and is not moved or dropped while this server loop is running.
        unsafe {
            let mut count: u32 = 0;
            while (*driver_ptr).is_busy() {
                count = count.wrapping_add(1);
                if count % 1000 == 0 {
                    let status_val = (*driver_ptr).read_status().unwrap_or(0xff);
                    pw_log::info!("SPI Flash busy... count={}, status=0x{:02x}", count, status_val);
                }
                let _ = sleep_until(SystemClock::now() + Duration::from_millis(1));
            }
        }
    }
}

fn run_server() -> Result<(), ErrorCode> {
    pw_log::info!("combined_server: initializing EFlash driver");
    let mut eflash_driver =
        EmbeddedFlash::new_with_interrupts(unsafe { flash_ctrl_core::FlashCtrl::new() });
    eflash_driver.set_default_permission(Permission::FULL_ACCESS);
    // Grant info page permissions as well (same as standard eflash server)
    for i in 5..9 {
        eflash_driver.set_info_permission(FlashAddress::info(0, i, 0), Permission::FULL_ACCESS)?;
        eflash_driver.set_info_permission(FlashAddress::info(1, i, 0), Permission::FULL_ACCESS)?;
    }

    let mut eflash = BlockingFlash {
        driver: eflash_driver,
        blocking: FlashCtrlInterrupt,
    };
    let mut eflash_server = FlashIpcServer::new(&mut eflash);

    pw_log::info!("combined_server: initializing SPI Host");
    let spi_host0 = unsafe { SpiHost0::new() };
    let mut spi_host = earlgrey_spi_host::SpiHost::new(spi_host0);
    spi_host.init().map_err(|_| KERNEL_ERROR_INTERNAL)?;

    pw_log::info!("combined_server: initializing SpiFlash driver");
    let mut spi_flash_driver = SpiFlash::new(spi_host);
    spi_flash_driver.init().map_err(|_| KERNEL_ERROR_INTERNAL)?;

    let blocking = SpiFlashBlocking::new();
    let mut spi_flash = BlockingFlash {
        driver: spi_flash_driver,
        blocking,
    };
    let driver_ptr = &mut spi_flash.driver as *mut _;
    spi_flash.blocking.set_driver(driver_ptr);
    let mut spi_flash_server = FlashIpcServer::new(&mut spi_flash);

    pw_log::info!("combined_server: registering wait group ports");
    syscall::wait_group_add(
        handle::FLASH_WAIT_GROUP,
        handle::EFLASH_SERVICE,
        syscall::Signals::READABLE,
        1, // token 1 = EFlash
    )
    .map_err(ErrorCode::kernel_error)?;

    syscall::wait_group_add(
        handle::FLASH_WAIT_GROUP,
        handle::SPI_FLASH_SERVICE,
        syscall::Signals::READABLE,
        2, // token 2 = SPI Flash
    )
    .map_err(ErrorCode::kernel_error)?;

    let mut buf = [0u8; 2064];
    let eflash_ipc = IpcHandle::new(handle::EFLASH_SERVICE);
    let spi_flash_ipc = IpcHandle::new(handle::SPI_FLASH_SERVICE);

    pw_log::info!("combined_server: entering main wait_group loop");
    loop {
        let wait_result = syscall::object_wait(
            handle::FLASH_WAIT_GROUP,
            syscall::Signals::READABLE,
            Instant::MAX,
        )
        .map_err(ErrorCode::kernel_error)?;

        let token = wait_result.user_data;
        if token == 1 {
            eflash_server.handle_one(&eflash_ipc, &mut buf)?;
        } else if token == 2 {
            spi_flash_server.handle_one(&spi_flash_ipc, &mut buf)?;
        }
    }
}

#[entry]
fn entry() -> Result<(), Error> {
    pw_log::info!("🔄 COMBINED FLASH SERVER START");
    let ret = run_server();

    let ret = match ret {
        Ok(()) => {
            pw_log::info!("✅ COMBINED FLASH SERVER PASS");
            Ok(())
        }
        Err(e) => {
            pw_log::error!("❌ COMBINED FLASH SERVER FAIL: {:08x}", u32::from(e));
            Err(Error::Unknown)
        }
    };
    ret
}

util_panic::make_panic_handler!();

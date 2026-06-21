// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! SPI Flash IPC client for integration testing.

#![no_std]
#![no_main]

use pw_status::Error;
use spi_flash_test_codegen::handle;
use userspace::entry;

use earlgrey_util::EarlgreyFlashAddress;
use hal_flash::{Flash, FlashAddress};
use services_flash_client::FlashIpcClient;
use util_error::ErrorCode;
use util_ipc::IpcHandle;

fn erase_program_test(flash: &mut FlashIpcClient, addr: FlashAddress) -> Result<(), ErrorCode> {
    let (_total_size, page_size, _erasable_sizes_bitmap) = flash.geometry()?;

    pw_log::info!(
        "spi_flash_test: Erasing 4KB at offset 0x{:08x}",
        addr.offset()
    );
    flash.erase(addr, page_size)?;

    pw_log::info!("spi_flash_test: Reading back to verify erase");
    let mut buf = [0u8; 32];
    flash.read(addr, &mut buf)?;
    util_misc::hexdump(&buf);
    for &b in &buf {
        if b != 0xFF {
            pw_log::error!(
                "spi_flash_test: Erase failed, byte is not 0xFF: 0x{:02x}",
                b
            );
            return Err(ErrorCode::kernel_error(pw_status::Error::Internal));
        }
    }

    let test_data = b"Maize SPI Flash IPC Test!";
    pw_log::info!("spi_flash_test: Programming test data");
    flash.program(addr, test_data)?;

    pw_log::info!("spi_flash_test: Reading back to verify program");
    let mut read_buf = [0u8; 32];
    flash.read(addr, &mut read_buf)?;
    util_misc::hexdump(&read_buf);

    let read_slice = read_buf
        .get(..test_data.len())
        .ok_or_else(|| ErrorCode::kernel_error(pw_status::Error::Internal))?;
    if read_slice != test_data {
        pw_log::error!("spi_flash_test: Verification failed!");
        return Err(ErrorCode::kernel_error(pw_status::Error::Internal));
    }

    pw_log::info!("spi_flash_test: Verification successful!");
    Ok(())
}

fn spi_flash_test() -> Result<(), ErrorCode> {
    pw_log::info!("spi_flash_test: connecting to service");
    let mut flash = FlashIpcClient::new(IpcHandle::new(handle::SPI_FLASH_SERVICE))?;

    let (total_size, page_size, _erasable_sizes_bitmap) = flash.geometry()?;
    pw_log::info!("spi_flash_test: Flash size: {} bytes", total_size.get());
    pw_log::info!("spi_flash_test: Flash page size: {} bytes", page_size.get());

    erase_program_test(&mut flash, FlashAddress::data(0x100000))?;
    Ok(())
}

#[entry]
fn entry() -> Result<(), Error> {
    pw_log::info!("🔄 SPI FLASH TEST START");
    let ret = spi_flash_test();

    let ret = match ret {
        Ok(()) => {
            pw_log::info!("✅ SPI FLASH TEST PASS");
            Ok(())
        }
        Err(e) => {
            pw_log::error!("❌ SPI FLASH TEST FAIL: {:08x}", u32::from(e) as u32);
            Err(Error::Unknown)
        }
    };
    ret
}

util_panic::make_panic_handler!();

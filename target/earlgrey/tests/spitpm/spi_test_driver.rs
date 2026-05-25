// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_std]
#![no_main]

use pw_status::Result;
use ureg;
use userspace::time::{Clock, SystemClock, Duration};
use userspace::{entry, syscall};
use registers::spi_host::*;

const DIR_RXONLY: u32 = 1;
const DIR_TXONLY: u32 = 2;

// 1. Initialize physical SPI Host register block (including output_en physical switch fix!)
fn init_spi_host(regs: &mut RegisterBlock<ureg::RealMmioMut<'_>>) {
    // Write to Configopts register, configuring clocks and safe CS timings
    regs.configopts().write(|w| {
        w.clkdiv(1)
         .cpha(false)
         .cpol(false)
         .fullcyc(true)
         .csnlead(0)
         .csnidle(2)
         .csntrail(1)
     });

    // 🔴 Fix: Enable SPI Host (spien) and "Enable physical pin output buffers (output_en)"!
    regs.control().write(|w| w.spien(true).output_en(true));
    
    // Configure Chip Select ID to 0
    regs.csid().write(|_| 0);
    
    pw_log::info!("📟 Host Driver: SPI Host0 initialized with output buffers enabled!");
}

// 2. Transmit a data slice by writing to the COMMAND register to trigger FSM actively
fn spi_host_write(regs: &mut RegisterBlock<ureg::RealMmioMut<'_>>, data: &[u8], csaat: bool) {
    // Ensure the output buffers are enabled before transmitting
    regs.control().modify(|w| w.output_en(true));

    for b in data.iter() {
        regs.txdata().write(|_| *b as u32);
    }
    regs.command().write(|w| {
        w.direction(DIR_TXONLY)
         .csaat(csaat)
         .len(data.len() as u32 - 1)
    });
    while !regs.status().read().txempty() {}
    
    // If transaction is complete (CS raised high), disable output buffers to avoid interference
    if !csaat {
        regs.control().modify(|w| w.output_en(false));
    }
}

// 3. Drive the clock to receive data by writing to the COMMAND register actively
fn spi_host_read(regs: &mut RegisterBlock<ureg::RealMmioMut<'_>>, out: &mut [u8]) {
    // Ensure output buffers are enabled before reading
    regs.control().modify(|w| w.output_en(true));

    regs.command().write(|w| {
        w.direction(DIR_RXONLY)
         .csaat(false)
         .len(out.len() as u32 - 1)
    });
    for i in 0..out.len() {
        while regs.status().read().rxempty() {}
        out[i] = regs.rxdata().read() as u8;
    }
    
    // Close output buffers and release CS after reading is complete
    regs.control().modify(|w| w.output_en(false));
}

#[entry]
fn entry() -> Result<()> {
    pw_log::info!("📟 Host Driver: SPI Test Driver starting...");

    let mut spi_host0 = unsafe { SpiHost0::new() };
    let mut regs = spi_host0.regs_mut();

    // Call Host initialization
    init_spi_host(&mut regs);

    // Wait for 500ms to let the Bridge complete initialization
    let start_time = SystemClock::now();
    let delay = Duration::from_millis(500);
    while SystemClock::now() - start_time < delay {}

    // ===================================================
    // 📥 Attack Phase: Host transmits TPM write command
    // ===================================================
    pw_log::info!("📟 Host Driver: Sending TPM Write FIFO header [0x03, 0x00, 0x00, 0x24]...");
    let header = [0x03, 0x00, 0x00, 0x24];
    spi_host_write(&mut regs, &header, true); 

    pw_log::info!("📟 Host Driver: Sending 4 bytes Payload [0xDE, 0xAD, 0xBE, 0xEF]...");
    let payload = [0xDE, 0xAD, 0xBE, 0xEF];
    spi_host_write(&mut regs, &payload, false); 

    // Wait for 500ms to let the Bridge and Mock TPM Service finish routing and calculation
    let wait_time = SystemClock::now();
    while SystemClock::now() - wait_time < delay {}

    // ===================================================
    // 📤 Harvest Phase: Host queries TPM response via SPI Read FIFO!
    // ===================================================
    pw_log::info!("📟 Host Driver: Querying TPM response via SPI Read FIFO [0x83, 0x00, 0x00, 0x24]...");
    let read_header = [0x83, 0x00, 0x00, 0x24];
    spi_host_write(&mut regs, &read_header, true); 

    // Allocate receiving buffer and trigger reading
    let mut response = [0u8; 4];
    spi_host_read(&mut regs, &mut response);
    
    pw_log::info!("📟 Host Driver: Received SPI Response Payload: [0x{:02X}, 0x{:02X}, 0x{:02X}, 0x{:02X}]", response[0], response[1], response[2], response[3]);

    // Verify the returned payload matches what was written (Closed-loop Echo Verification!)
    assert_eq!(response, [0xDE, 0xAD, 0xBE, 0xEF]);
    
    pw_log::info!("✅ PASSED: Loopback dynamic transaction is 100% verified!");

    // Signal the microkernel to shutdown gracefully!
    let _ = syscall::debug_shutdown(Ok(()));

    Ok(())
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    pw_log::error!("FAIL: panic in spi_test_driver!");
    let _ = syscall::debug_shutdown(Err(pw_status::Error::Unknown));
    loop {}
}

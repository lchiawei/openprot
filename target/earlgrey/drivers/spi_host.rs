// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

//! SPI Host driver for Earlgrey.
//!
//! This driver implements the `embedded-hal` 1.0 SPI traits for the Earlgrey SPI Host controller.

#![no_std]
#![allow(clippy::unnecessary_cast)]

/// SPI Host driver errors.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SpiError {
    /// Invalid transaction parameters or state.
    InvalidTransaction,
    /// TX FIFO overflow.
    FifoOverflow,
    /// RX FIFO underflow.
    FifoUnderflow,
    /// Operation timed out.
    Timeout,
    /// Hardware error reported by the controller.
    HardwareError,
}

impl embedded_hal::spi::Error for SpiError {
    fn kind(&self) -> embedded_hal::spi::ErrorKind {
        // Map all errors to Other for now.
        embedded_hal::spi::ErrorKind::Other
    }
}

/// Earlgrey SPI Host driver.
pub struct SpiHost {
    registers: spi_host::RegisterBlock<ureg::RealMmioMut<'static>>,
    base_ptr: *mut u32,
}

impl SpiHost {
    /// Create a new SpiHost instance for SPI_HOST0.
    ///
    /// # Safety
    ///
    /// The caller must ensure they have exclusive access to the SPI_HOST0 peripheral.
    pub unsafe fn new_spi0() -> Self {
        Self {
            registers: unsafe { spi_host::RegisterBlock::new(spi_host::SpiHost0::PTR) },
            base_ptr: spi_host::SpiHost0::PTR,
        }
    }

    /// Initialize the SPI Host peripheral.
    ///
    /// Configures the clock, SPI mode (0), CS timings, performs a reset,
    /// and enables the peripheral.
    pub fn init(&mut self) -> Result<(), SpiError> {
        // Core clock divider. Slows down subsequent SPI transactions by a
        // factor of (CLKDIV+1) relative to the core clock frequency.
        // If core clock is 96MHz, clkdiv = 1 yields 24MHz SPI clock.
        let clkdiv = 1;

        self.registers.configopts().write(|w| {
            w.clkdiv(clkdiv)
                .cpha(false)
                .cpol(false)
                .fullcyc(true)
                .csnlead(0)
                .csnidle(2)
                .csntrail(1)
        });

        // Reset the peripheral.
        self.registers.control().write(|w| w.sw_rst(true));

        // Wait for both FIFOs to empty.
        let mut timeout = 1_000_000;
        loop {
            let status = self.registers.status().read();
            if status.txempty() && status.rxempty() {
                break;
            }
            timeout -= 1;
            if timeout == 0 {
                return Err(SpiError::Timeout);
            }
        }

        // Release reset and enable SPI host.
        self.registers
            .control()
            .write(|w| w.sw_rst(false).spien(true));

        // Default to CS 0.
        self.registers.csid().write(|_| 0);

        Ok(())
    }

    fn is_ready(&self) -> bool {
        self.registers.status().read().ready()
    }

    /// Check if the SPI Host is currently active processing a command.
    fn is_active(&self) -> bool {
        self.registers.status().read().active()
    }

    /// Write data to TX FIFO. Uses 32-bit writes for performance and to prevent
    /// FIFO entry overflow, and 8-bit writes for the remainder.
    fn write_fifo(&self, data: &[u8]) {
        let txdata_u32 = self.base_ptr.wrapping_add(0x28 / 4);
        let txdata_u8 = (self.base_ptr as *mut u8).wrapping_add(0x28);

        let mut chunks = data.chunks_exact(4);
        for chunk in &mut chunks {
            if let [b0, b1, b2, b3] = chunk {
                let word = u32::from_le_bytes([*b0, *b1, *b2, *b3]);
                unsafe {
                    core::ptr::write_volatile(txdata_u32, word);
                }
            }
        }

        let remainder = chunks.remainder();
        for &b in remainder {
            unsafe {
                core::ptr::write_volatile(txdata_u8, b);
            }
        }
    }

    /// Send a write command segment.
    fn write_cmd(&self, mut data: &[u8], speed: u32, final_csaat: bool) {
        let regs = &self.registers;
        regs.control().modify(|w| w.output_en(true));

        while !data.is_empty() {
            let mut ready_count = 0;
            while !self.is_ready() {
                ready_count += 1;
                if ready_count % 100000 == 0 {
                    let status = regs.status().read();
                    pw_log::warn!(
                        "write_cmd: waiting for ready... count={}, raw_status=0x{:08x}",
                        ready_count as u32,
                        status.0 as u32
                    );
                }
            }

            let mut txqd_count = 0;
            while regs.status().read().txqd() != 0 {
                txqd_count += 1;
                if txqd_count % 100000 == 0 {
                    let status = regs.status().read();
                    pw_log::warn!(
                        "write_cmd: waiting for txqd==0... count={}, raw_status=0x{:08x}",
                        txqd_count as u32,
                        status.0 as u32
                    );
                }
            }

            let chunk_len = core::cmp::min(data.len(), MAX_TX_CHUNK_LEN);
            let chunk = &data[..chunk_len];
            data = &data[chunk_len..];

            self.write_fifo(chunk);

            let chunk_csaat = !data.is_empty() || final_csaat;

            regs.command().write(|w| {
                w.speed(speed)
                    .csaat(chunk_csaat)
                    .direction(DIR_TXONLY)
                    .len(chunk_len.saturating_sub(1) as u32)
            });
        }
    }

    /// Send a read command segment and receive data into `dest`.
    fn read_cmd(
        &self,
        mut len: usize,
        speed: u32,
        final_csaat: bool,
        mut dest: &mut [u8],
    ) -> Result<(), SpiError> {
        let regs = &self.registers;
        regs.control().modify(|w| w.output_en(true));

        while len > 0 {
            let mut ready_count = 0;
            while !self.is_ready() {
                ready_count += 1;
                if ready_count % 100000 == 0 {
                    let status = regs.status().read();
                    pw_log::warn!(
                        "read_cmd: waiting for ready... count={}, raw_status=0x{:08x}",
                        ready_count as u32,
                        status.0 as u32
                    );
                }
            }

            let chunk_len = core::cmp::min(len, MAX_RX_CHUNK_LEN);
            len -= chunk_len;

            let chunk_csaat = len > 0 || final_csaat;

            regs.command().write(|w| {
                w.speed(speed)
                    .csaat(chunk_csaat)
                    .direction(DIR_RXONLY)
                    .len(chunk_len.saturating_sub(1) as u32)
            });

            // Split dest into the chunk we want to read now, and the rest for later.
            // Using split_at_mut avoids the borrow checker conflict.
            let (mut chunk_dest, rest) = dest.split_at_mut(chunk_len);
            dest = rest;

            let mut rx_wait_count = 0;
            while !chunk_dest.is_empty() {
                let status = regs.status().read();
                let rxqd = status.rxqd() as usize; // rxqd is in 32-bit words
                let bytes_in_fifo = rxqd.checked_mul(4).ok_or(SpiError::HardwareError)?;

                if bytes_in_fifo > 0 {
                    let drain_len = core::cmp::min(bytes_in_fifo, chunk_dest.len());
                    let (drain_chunk, remaining) = chunk_dest.split_at_mut(drain_len);
                    util_regcpy::copy_from_reg_unaligned(drain_chunk, &regs.rxdata());
                    chunk_dest = remaining;
                    rx_wait_count = 0; // reset
                } else {
                    rx_wait_count += 1;
                    if rx_wait_count % 100000 == 0 {
                        pw_log::warn!(
                            "read_cmd: waiting for RX data... count={}, raw_status=0x{:08x}",
                            rx_wait_count as u32,
                            status.0 as u32
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

const MAX_RX_CHUNK_LEN: usize = 256;
const MAX_TX_CHUNK_LEN: usize = 288;
const DIR_RXONLY: u32 = 1;
const DIR_TXONLY: u32 = 2;

impl embedded_hal::spi::ErrorType for SpiHost {
    type Error = SpiError;
}

impl embedded_hal::spi::SpiDevice for SpiHost {
    fn transaction(
        &mut self,
        operations: &mut [embedded_hal::spi::Operation<'_, u8>],
    ) -> Result<(), Self::Error> {
        let op_count = operations.len();
        for (i, op) in operations.iter_mut().enumerate() {
            let is_last = i == op_count - 1;
            let csaat = !is_last;

            match op {
                embedded_hal::spi::Operation::Read(buf) => {
                    self.read_cmd(buf.len(), 0, csaat, buf)?;
                }
                embedded_hal::spi::Operation::Write(buf) => {
                    self.write_cmd(buf, 0, csaat);
                }
                embedded_hal::spi::Operation::Transfer(_, _)
                | embedded_hal::spi::Operation::TransferInPlace(_) => {
                    // Full-duplex SPI transfer is not supported by this simple driver yet.
                    return Err(SpiError::InvalidTransaction);
                }
                embedded_hal::spi::Operation::DelayNs(_) => {
                    // Delay operation is not supported yet.
                    return Err(SpiError::InvalidTransaction);
                }
            }
        }

        // Wait for the controller to finish the last command segment.
        while self.is_active() {}
        // Disable output buffers to release the bus.
        self.registers.control().modify(|w| w.output_en(false));

        Ok(())
    }
}

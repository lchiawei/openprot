// Licensed under the Apache-2.0 license
// SPDX-License-Identifier: Apache-2.0

#![no_std]
#![no_main]

use mock_tpm_codegen::handle;
// use pw_status::{Error, Result};
use pw_status::Result;
use userspace::syscall::Signals;
use userspace::time::Instant;
use userspace::{entry, syscall};

// Simple hex dump logging utility
fn hexdump(data: &[u8]) {
    for chunk in data.chunks(16) {
        for b in chunk {
            pw_log::info!("0x{:02x} ", *b);
        }
    }
}

fn handle_ipc() -> Result<()> {
    let mut cmd_buf = [0u8; 1024];

    loop {
        // 1. Block waiting for IPC channel signal
        let wait_return = syscall::object_wait(handle::IPC, Signals::READABLE, Instant::MAX)?;

        if !wait_return.pending_signals.contains(Signals::READABLE) {
            continue;
        }

        // 2. Read TPM command data from the Bridge
        let cmd_len = syscall::channel_read(handle::IPC, 0, &mut cmd_buf)?;
        if cmd_len == 0 {
            continue;
        }

        pw_log::info!("🔮 Mock TPM: Received Command Payload (len={})", cmd_len);
        hexdump(&cmd_buf[..cmd_len]);

        // 3. Simulate computation: directly echo the received command as response back to the channel
        pw_log::info!("🔮 Mock TPM: Echoing payload back to bridge...");
        syscall::channel_respond(handle::IPC, &cmd_buf[..cmd_len])?;
    }
}

#[entry]
fn entry() -> Result<()> {
    pw_log::info!("🔮 Mock TPM: Service starting...");

    let ret = handle_ipc();
    if let Err(e) = ret {
        pw_log::error!("❌ Mock TPM: IPC handler failed: {}", e as u32);
    }

    let _ = syscall::debug_shutdown(ret);
    ret
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    pw_log::error!("FAIL: panic in mock_tpm!");
    let _ = syscall::debug_shutdown(Err(pw_status::Error::Unknown));
    loop {}
}

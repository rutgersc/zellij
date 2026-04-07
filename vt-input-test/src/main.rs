//! Minimal test: does ENABLE_VIRTUAL_TERMINAL_INPUT deliver arrow key bytes?
//!
//! Run inside Windows Terminal. Press arrow keys, letters, Ctrl+Shift+F, etc.
//! Each keypress should print the raw bytes received via ReadFile on stdin.

use std::io::{BufRead, Read};

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_EXTENDED_FLAGS,
    ENABLE_VIRTUAL_TERMINAL_INPUT, STD_INPUT_HANDLE,
};

fn format_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| {
            if *b >= 0x20 && *b < 0x7f {
                format!("{}", *b as char)
            } else {
                format!("\\x{:02x}", b)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let use_fill_buf = mode == "--fill-buf";

    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    assert!(handle != 0 && handle != INVALID_HANDLE_VALUE, "no stdin handle");

    let mut original_mode: u32 = 0;
    unsafe { GetConsoleMode(handle, &mut original_mode) };
    println!("Original console mode: 0x{:04x}", original_mode);

    let new_mode = ENABLE_EXTENDED_FLAGS | ENABLE_VIRTUAL_TERMINAL_INPUT;
    let ok = unsafe { SetConsoleMode(handle, new_mode) };
    assert!(ok != 0, "SetConsoleMode failed");

    let mut verify_mode: u32 = 0;
    unsafe { GetConsoleMode(handle, &mut verify_mode) };
    println!("New console mode:      0x{:04x}", verify_mode);
    println!("Reader mode:           {}", if use_fill_buf { "fill_buf (like zellij)" } else { "read" });
    println!();
    println!("Press keys (arrows, letters, Esc, backspace, etc.)");
    println!("Press Ctrl+C to exit.");
    println!();

    let mut stdin = std::io::stdin().lock();

    if use_fill_buf {
        loop {
            let bytes = match stdin.fill_buf() {
                Ok(b) if b.is_empty() => break,
                Ok(b) => b.to_vec(),
                Err(e) => { eprintln!("fill_buf error: {}", e); break; },
            };
            let n = bytes.len();
            println!("  {} bytes: {:?}  ({})", n, &bytes, format_bytes(&bytes));
            stdin.consume(n);
        }
    } else {
        let mut buf = [0u8; 64];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let bytes = &buf[..n];
                    println!("  {} bytes: {:?}  ({})", n, bytes, format_bytes(bytes));
                },
                Err(e) => { eprintln!("read error: {}", e); break; },
            }
        }
    }

    unsafe { SetConsoleMode(handle, original_mode) };
}

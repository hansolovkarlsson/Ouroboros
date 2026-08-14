#![no_main]
#![no_std]

extern crate alloc;

use core::time::Duration;
use uefi::prelude::*;
use uefi::boot;

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    log::info!("Ouroboros kernel: UEFI stage alive");

    boot::stall(Duration::from_secs(3));

    Status::SUCCESS
}

# Build an ARM64 OS from scratch: the working path

This is a tutorial distilled from building [Ouroboros](../README.md) — a
from-scratch aarch64 microkernel in Rust that boots via UEFI, runs a
preemptively-scheduled userland shell loaded from disk, and works on
both QEMU and real Parallels-on-Apple-Silicon hardware. Unlike the
[postmortems](boot-bringup-postmortem.md), which document the debugging
that *found* this path, this document presents only the path itself:
each stage as the working design, with real code from the finished
kernel, and a short "why this way" note wherever the working choice is
non-obvious. Follow the stages in order — each one is independently
bootable and testable, which is the single most important property of
the whole approach.

**What you need:** a Mac or Linux machine, stable Rust (`rustup`), QEMU
(`brew install qemu` — this also installs the aarch64 UEFI firmware),
and no prior kernel experience beyond comfort with Rust and a
willingness to read ARM documentation. No nightly toolchain, no
`build-std`, no cross-compiler to set up — both targets used here ship
prebuilt `core`/`alloc` on stable Rust.

**The three principles the whole path rests on:**

1. **Discover, never hardcode.** Every address — the console, the
   interrupt controller, RAM itself — comes from something the platform
   tells you (ACPI tables, the UEFI memory map, PCI config space),
   never from a constant that happened to work on one emulator.
   Hardcoded addresses are the single biggest source of "works on QEMU,
   crashes on real hardware."
2. **Each stage proves itself before the next begins.** "It compiles"
   and even "it boots" prove little in kernel code; every stage below
   ends with a specific observable behavior to confirm.
3. **When a choice looks arbitrary, match what firmware already does.**
   You inherit a running configuration (MMU on, translation tables
   live, an exception level). Diverging from firmware's configuration
   mid-flight is where the deepest problems live; matching it is almost
   always the working answer.

---

## Stage 0: Project setup — your kernel *is* a UEFI application

Skip the classic bootloader entirely. UEFI firmware will load a PE/COFF
executable from a FAT filesystem for you, with your code running at EL1
with the MMU on and a working console — that's a better starting point
than a bare assembly boot stub, and it's the *only* boot path some real
platforms (Parallels among them) offer at all.

Create a workspace with the kernel as a UEFI application:

```toml
# Cargo.toml (workspace root)
[workspace]
resolver = "2"
members = ["kernel"]
default-members = ["kernel"]

[profile.dev]
panic = "abort"

[profile.release]
panic = "abort"
```

```toml
# .cargo/config.toml
[build]
target = "aarch64-unknown-uefi"
```

```toml
# kernel/Cargo.toml
[package]
name = "kernel"
edition = "2021"

[dependencies]
uefi = { version = "0.39", features = ["alloc", "global_allocator", "logger", "panic_handler"] }
log = "0.4"

[[bin]]
name = "BOOTAA64"
path = "src/main.rs"
```

The `[[bin]]` name is doing real work: UEFI firmware auto-boots
removable media from the fixed path `\EFI\BOOT\BOOTAA64.EFI`, so naming
the binary `BOOTAA64` means the build output stages straight into a
bootable disk layout with no rename step.

The minimal kernel:

```rust
// kernel/src/main.rs
#![no_main]
#![no_std]

extern crate alloc;

use uefi::prelude::*;

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();
    log::info!("kernel: UEFI stage alive");
    halt()
}

/// Parks the core forever instead of returning to firmware. `wfe` is a
/// low-power spin (wait-for-event) rather than a busy loop.
fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}
```

Note that `main` never returns `Status::SUCCESS` — returning hands
control back to firmware, which is a dead end for a kernel. Park the
core instead.

Stage the binary and boot it:

```sh
cargo build
mkdir -p esp/EFI/BOOT
cp target/aarch64-unknown-uefi/debug/BOOTAA64.efi esp/EFI/BOOT/BOOTAA64.EFI

qemu-system-aarch64 \
    -machine virt -cpu cortex-a72 -m 512M \
    -bios "$(brew --prefix qemu)/share/qemu/edk2-aarch64-code.fd" \
    -drive file=fat:rw:esp,format=raw,media=disk,if=none,id=hd0 \
    -device virtio-blk-device,drive=hd0 \
    -nographic
```

**Confirm:** the log line appears on QEMU's serial console. You have a
kernel. Put the QEMU invocation in a Makefile now — you will run it
hundreds of times.

---

## Stage 1: Find your console — then leave boot services

Everything so far prints through UEFI's own console, which dies the
moment you call `exit_boot_services` — and you must eventually call it,
because boot services own the machine (memory, timers, drivers) until
you do. So the first real driver you write is a serial console, and the
first real *discovery* you do is finding its address.

**Why discovery instead of QEMU's well-known PL011 address
(`0x0900_0000`):** nothing guarantees that address exists on any other
platform, and writing to an unmapped device address doesn't fail
politely — it can take down the whole VM. The working rule: *no
confirmed address means no console output, not a guess.*

The mechanism that works on ACPI platforms (QEMU's firmware and most
real firmware) is the **SPCR table** ("Serial Port Console
Redirection"): firmware's own declaration of where its console UART
lives. The walk is three fixed-offset struct reads — RSDP → XSDT → scan
for the table you want — and needs no ACPI crate:

```rust
// kernel/src/acpi.rs (condensed)
use core::mem::size_of;
use core::ptr;
use uefi::table::cfg::ConfigTableEntry;

/// Finds the ACPI RSDP pointer via the UEFI configuration table.
/// Must be called before exit_boot_services.
pub fn find_rsdp() -> Option<*const u8> {
    uefi::system::with_config_table(|entries| {
        entries.iter()
            .find(|e| e.guid == ConfigTableEntry::ACPI2_GUID)
            .map(|e| e.address.cast::<u8>())
    })
}

#[repr(C, packed)]
struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

#[repr(C, packed)]
pub struct SdtHeader {
    pub signature: [u8; 4],
    pub length: u32,
    // ... revision/checksum/OEM fields, 36 bytes total
}

/// Walks RSDP -> XSDT for a table matching `signature` (b"SPCR" here;
/// the same walk later finds b"APIC" — the MADT — for interrupts).
pub unsafe fn find_table(rsdp: *const u8, signature: &[u8; 4])
    -> Option<*const u8>
{
    let rsdp = ptr::read_unaligned(rsdp.cast::<Rsdp>());
    if &rsdp.signature != b"RSD PTR " || rsdp.revision < 2 {
        return None;
    }
    let xsdt_ptr = rsdp.xsdt_address as *const u8;
    let xsdt = ptr::read_unaligned(xsdt_ptr.cast::<SdtHeader>());
    let count = (xsdt.length as usize - size_of::<SdtHeader>()) / 8;
    let entries = xsdt_ptr.add(size_of::<SdtHeader>()).cast::<u64>();
    for i in 0..count {
        let table = ptr::read_unaligned(entries.add(i)) as *const u8;
        let header = ptr::read_unaligned(table.cast::<SdtHeader>());
        if &header.signature == signature {
            return Some(table);
        }
    }
    None
}
```

The SPCR table's body then gives you an interface type (check for PL011
or the register-compatible SBSA Generic UART) and a base address. Use
`read_unaligned` for every ACPI struct read — the tables carry no
alignment guarantees, and packed-struct field access through references
is undefined behavior in Rust.

The PL011 driver itself is two registers:

```rust
// kernel/src/uart.rs
use core::fmt;
use core::ptr::{read_volatile, write_volatile};

const DR_OFFSET: usize = 0x00;  // data register
const FR_OFFSET: usize = 0x18;  // flag register
const FR_TXFF: u32 = 1 << 5;    // transmit FIFO full
const FR_RXFE: u32 = 1 << 4;    // receive FIFO empty

pub struct Uart { base: usize }

impl Uart {
    /// # Safety
    /// `base` must be the MMIO base of a real, mapped PL011.
    pub unsafe fn new(base: usize) -> Self { Uart { base } }

    pub fn write_byte(&mut self, byte: u8) {
        unsafe {
            let fr = (self.base + FR_OFFSET) as *const u32;
            while read_volatile(fr) & FR_TXFF != 0 {}
            let dr = (self.base + DR_OFFSET) as *mut u32;
            write_volatile(dr, byte as u32);
        }
    }

    /// Non-blocking: None if nothing is waiting.
    pub fn read_byte(&mut self) -> Option<u8> {
        unsafe {
            let fr = (self.base + FR_OFFSET) as *const u32;
            if read_volatile(fr) & FR_RXFE != 0 { return None; }
            let dr = (self.base + DR_OFFSET) as *const u32;
            Some(read_volatile(dr) as u8)
        }
    }
}

impl fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' { self.write_byte(b'\r'); }
            self.write_byte(byte);
        }
        Ok(())
    }
}
```

The boot sequence ordering that works:

```rust
// in main(), in this order:
let rsdp = acpi::find_rsdp();                     // needs boot services
let console_base = discover_console(rsdp);        // parse + log result
let memory_map = unsafe {
    uefi::boot::exit_boot_services(None)          // keep the map! stage 3 needs it
};
// From here: no log::*, no alloc, no UEFI anything. Only your own drivers.
if let Some(base) = console_base {
    console::install(unsafe { Uart::new(base) }); // a global the kernel prints through
}
```

Two rules embedded there: **do all discovery and log its results before
the exit** (so failures are visible on a console that still works), and
**keep the memory map `exit_boot_services` returns** — it's the only
description of RAM you will ever get, and the MMU stage is built on it.
Wrap the installed console in a small global (an
`UnsafeCell<Option<Console>>` with a `println!` macro over it — fine on
a single core) so everything after this point, including the exception
handler you're about to write, has somewhere to print.

**Confirm:** a post-exit line ("console live") appears via your own
driver, at an address you discovered rather than assumed.

---

## Stage 2: Exception vectors — before anything else can fault

With boot services gone, a bad memory access has nowhere to go: an
unconfigured `VBAR_EL1` turns your first pointer bug into a silent
platform-level crash with zero diagnostics. Install a vector table
*immediately* after `exit_boot_services`, before any other post-exit
code runs.

AArch64's table is 16 slots (4 exception classes × 4 source groups),
each slot 0x80 bytes, the table itself 0x800-aligned. The minimal
working version sends every slot to one reporting handler:

```rust
// kernel/src/exceptions.rs (minimal first version)
use core::arch::global_asm;
use core::ffi::c_void;

global_asm!(
    r#"
.text
.balign 0x800
.global exception_vector_table
exception_vector_table:

.balign 0x80
mov x3, #0
b   1f
.balign 0x80
mov x3, #1
b   1f
// ... one 0x80-aligned stub per slot, 16 total, each loading its
// index into x3 ...

1:
mrs x0, esr_el1     // what happened
mrs x1, far_el1     // at what address
mrs x2, elr_el1     // from which instruction
b   {rust_handler}
"#,
    rust_handler = sym rust_exception_handler,
);

unsafe extern "C" {
    static exception_vector_table: c_void;
}

pub fn install() {
    unsafe {
        let addr = &raw const exception_vector_table as u64;
        core::arch::asm!("msr vbar_el1, {0}", "isb", in(reg) addr);
    }
}

extern "C" fn rust_exception_handler(esr: u64, far: u64, elr: u64, vector: u64) -> ! {
    crate::console::println!(
        "EXCEPTION vector={vector} esr_el1={esr:#x} far_el1={far:#x} elr_el1={elr:#x}"
    );
    crate::halt()
}
```

**Why `.text` and not a custom section name:** the PE/COFF backend
marks `.text` executable but does not infer that for unrecognized
custom section names — a table in `.section .text.exceptions` links
fine, points `VBAR_EL1` at the right address, and then faults on its
own first instruction. Keep the `global_asm!` block in plain `.text`.

**Confirm — deliberately.** Write a temporary
`write_volatile(0 as *mut u8, 0xAB)` (address 0 is unmapped on QEMU's
`virt` machine), boot, and check that exactly one clean `EXCEPTION`
line prints and the machine halts stably. Run QEMU with `-d int -D
qemu.log` and check its own trace agrees: one Data Abort, no fault
loop. Then delete the test. That habit — cross-checking your kernel's
own reporting against QEMU's independent exception trace — is worth
keeping for every stage after this one; "zero unexpected aborts in the
`-d int` log" is the cheapest regression test a bare-metal project can
have.

Learn to decode `ESR_EL1` by hand now: bits 31:26 are the Exception
Class (0x15 = SVC, 0x21 = Instruction Abort, 0x25 = Data Abort), bits
5:0 the fault status code. Every hard problem later becomes tractable
the moment you can read these.

---

## Stage 3: Your own page tables — swapped live, matching firmware

Firmware hands you a running MMU on tables you don't control. A kernel
needs its own — for device mappings, for user/kernel permission
separation later — and the working technique is a **live swap**: build
a complete identity map, then repoint `TTBR0_EL1` at it with the MMU
continuously enabled, properly barriered. No disable/re-enable step.

The map that works as a first cut is deliberately coarse:

- **RAM**: every 1GB block overlapping the memory map's general-RAM
  descriptors (including the `LOADER_CODE`/`LOADER_DATA` your own image
  occupies!) mapped Normal, write-back cacheable, executable, EL1-only.
  The range comes from the **real memory map** you kept in stage 1 —
  never a constant.
- **Devices**: the low 1GB mapped Device-nGnRnE, non-executable — this
  covers the discovered console.

Descriptor construction, with the bit positions that matter:

```rust
// kernel/src/mmu.rs (condensed)
const DESC_VALID: u64 = 1 << 0;
const DESC_TABLE: u64 = 1 << 1;          // set = table; clear = block
const ATTRINDX_SHIFT: u64 = 2;           // index into MAIR_EL1
const AP_EL1_RW_ONLY: u64 = 0b00 << 6;   // AP[2:1]: EL1 R/W, no EL0
const AP_EL1_EL0_RW: u64 = 0b01 << 6;    // AP[2:1]: EL1+EL0 R/W
const SH_INNER: u64 = 0b11 << 8;
const SH_OUTER: u64 = 0b10 << 8;
const AF: u64 = 1 << 10;                 // access flag - set it, or fault
const PXN: u64 = 1 << 53;                // privileged execute-never
const UXN: u64 = 1 << 54;                // unprivileged execute-never

const MAIR_IDX_DEVICE_NGNRNE: u64 = 0;
const MAIR_IDX_NORMAL_WB: u64 = 1;

#[repr(align(4096))]
struct Table(UnsafeCell<[u64; 512]>);
unsafe impl Sync for Table {}            // single core, written before use

static L0_TABLE: Table = Table(UnsafeCell::new([0; 512]));
static L1_TABLE: Table = Table(UnsafeCell::new([0; 512]));

fn device_block(base: u64) -> u64 {
    (base & !(GIB - 1)) | DESC_VALID
        | (MAIR_IDX_DEVICE_NGNRNE << ATTRINDX_SHIFT)
        | AP_EL1_RW_ONLY | SH_OUTER | AF | PXN | UXN
}

fn normal_block(base: u64) -> u64 {
    (base & !(GIB - 1)) | DESC_VALID
        | (MAIR_IDX_NORMAL_WB << ATTRINDX_SHIFT)
        | AP_EL1_RW_ONLY | SH_INNER | AF | UXN
}

fn table_desc(next_level_addr: u64) -> u64 {
    DESC_VALID | DESC_TABLE | (next_level_addr & 0x0000_ffff_ffff_f000)
}
```

Source your bit positions from an authority — Linux's
`arch/arm64/include/asm/pgtable-hwdef.h` is the reference this kernel's
were checked against — because a wrong bit here often doesn't fault, it
silently mistranslates.

**The one configuration rule that matters most: match firmware's
starting level.** Read back firmware's `TCR_EL1` and use the same
`T0SZ` (44-bit VA, `T0SZ=20`, which walks from L0 through a two-level
L0→L1 structure, is what UEFI firmware uses on these platforms). A
single L1 table with `T0SZ=25` is architecturally legal and covers the
same addresses — and hard-faults when swapped in live on this
firmware's configuration. Changing attributes during a live swap works;
changing the walk's starting level does not. Take the two-level
structure as given and move on.

The switch sequence, fully barriered:

```rust
let tcr_el1: u64 = 20                // T0SZ: 44-bit VA, L0 start
    | (0b01 << 8) | (0b01 << 10)     // IRGN0/ORGN0: write-back
    | (0b11 << 12)                   // SH0: inner shareable
    | (1 << 23)                      // EPD1: no TTBR1 walks
    | (ips << 32);                   // IPS: from ID_AA64MMFR0_EL1, not guessed

unsafe {
    asm!(
        "msr daifset, #0xf",     // mask everything for the transition
        "dsb ishst",             // table writes visible before use
        "msr mair_el1, {mair}",
        "msr tcr_el1, {tcr}",
        "isb",
        "msr ttbr0_el1, {ttbr0}",
        "isb",
        "tlbi vmalle1",          // drop firmware's stale TLB entries
        "ic ialluis",            // and stale I-cache tags
        "dsb ish",
        "isb",
        mair = in(reg) mair_el1, tcr = in(reg) tcr_el1,
        ttbr0 = in(reg) l0_table_addr,
        options(nostack),
    );
}
```

**Confirm two ways:** print the discovered RAM span and mapped blocks;
then repeat stage 2's deliberate-fault test at an address your new
tables genuinely don't map, proving the exception handler still works
*under your own tables* — a different code path than before the switch.

---

## Stage 4: A timer tick — your first resumable exception

Everything so far halts on any exception. Preemption needs the
opposite: an interrupt that fires, gets handled, and *returns to what
it interrupted*. Two pieces of hardware and one new trampoline.

**The interrupt controller (GIC).** Discover its version and addresses
from the ACPI **MADT** table (signature `"APIC"` — the historical x86
name) using the same `find_table` walk from stage 1. The MADT's GICD
structure carries a `GIC Version` byte — that, not a guess, decides
whether you drive GICv2 (memory-mapped CPU interface) or GICv3
(system-register CPU interface plus per-CPU redistributors). QEMU's
default is GICv2, and its whole driver is this small:

```rust
// kernel/src/gicv2.rs (condensed)
const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER: usize = 0x100;   // + 4 * (intid / 32)
const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;         // priority mask
const GICC_IAR: usize = 0x00c;         // acknowledge (read)
const GICC_EOIR: usize = 0x010;        // end-of-interrupt (write)

pub unsafe fn init(gicd: usize, gicc: usize) {
    write_reg(gicd, GICD_CTLR, 1);         // enable distributor
    write_reg(gicc, GICC_PMR, 0xff);       // accept every priority
    write_reg(gicc, GICC_CTLR, 1);         // enable this CPU's interface
}

pub unsafe fn enable_interrupt(gicd: usize, intid: u32) {
    let reg = GICD_ISENABLER + 4 * ((intid / 32) as usize);
    write_reg(gicd, reg, 1u32 << (intid % 32));
}

pub unsafe fn acknowledge(gicc: usize) -> u32 { read_reg(gicc, GICC_IAR) }
pub unsafe fn end_of_interrupt(gicc: usize, intid: u32) {
    write_reg(gicc, GICC_EOIR, intid);
}
```

**The timer.** The ARM generic timer is pure system registers — no
MMIO, no platform-specific address, works on any ARMv8 CPU. Its
non-secure EL1 physical timer is PPI 14, which is GIC interrupt ID 30
(PPIs start at INTID 16):

```rust
// kernel/src/timer.rs (condensed)
pub const INTID: u32 = 30;
pub const TICK_INTERVAL_MS: u64 = 20;   // 20ms: responsive input later

pub fn frequency_hz() -> u64 {
    let f: u64;
    unsafe { asm!("mrs {0}, cntfrq_el0", out(reg) f) };
    f
}

/// One-shot: re-arming from the IRQ handler makes it periodic.
pub fn arm(interval_ms: u64) {
    let ticks = frequency_hz() / 1000 * interval_ms;
    unsafe {
        asm!(
            "msr cntp_tval_el0, {t}",
            "msr cntp_ctl_el0, {ctl}",
            t = in(reg) ticks, ctl = in(reg) 1u64,   // ENABLE
        );
    }
}
```

**The resumable IRQ trampoline.** The IRQ slot (index 5, "IRQ at
current EL with SP_ELx") gets its own path: save *all* general-purpose
registers plus `SP_EL0`, `ELR_EL1`, `SPSR_EL1` to the stack (272
bytes), call into Rust with a normal `bl`, restore everything, `eret`.
Design the saved frame's layout deliberately, because stage 6 will
reuse it as the task-switching mechanism itself:

```rust
/// The frame the IRQ trampoline saves — field order matches the
/// assembly's stack offsets byte for byte. A suspended task IS exactly
/// this much state.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Context {
    pub gpr: [u64; 31],   // x0-x30, offsets 0..248
    pub sp_el0: u64,      // offset 248
    pub elr_el1: u64,     // offset 256
    pub spsr_el1: u64,    // offset 264
}
```

```asm
2:                          // the IRQ slots branch here
sub sp, sp, #272
stp x0, x1, [sp, #0]
stp x2, x3, [sp, #16]
// ... x4-x29 in pairs ...
str x30, [sp, #240]
mrs x0, sp_el0
str x0, [sp, #248]
mrs x0, elr_el1
mrs x1, spsr_el1
stp x0, x1, [sp, #256]
mov x0, sp                  // hand the frame to Rust — this is the hook
bl  {rust_irq_handler}      // task switching hangs its hat on later
ldr x0, [sp, #248]
msr sp_el0, x0
ldp x0, x1, [sp, #256]
msr elr_el1, x0
msr spsr_el1, x1
// ... restore x0-x30 ...
add sp, sp, #272
eret
```

The Rust side acknowledges, counts, re-arms, completes:

```rust
extern "C" fn rust_irq_handler(frame: *mut Context) {
    let intid = unsafe { gic::acknowledge() };
    if intid == timer::INTID {
        TICKS.fetch_add(1, Ordering::Relaxed);
        timer::arm(timer::TICK_INTERVAL_MS);
        // stage 6 adds: tasks::on_tick(frame);
    }
    if intid != 1023 {                       // 1023 = spurious
        unsafe { gic::end_of_interrupt(intid) };
    }
}
```

Wire it up: `gic::init`, `gic::enable_interrupt(30)`, `timer::arm`,
then unmask IRQs (`msr daifclr, #2`) as the *last* thing before your
idle loop.

**Confirm sustained, not once:** print the tick counter for 20+ seconds.
A save/restore bug in the trampoline rarely breaks the first
round-trip — it corrupts state that only shows up after several. The
`-d int` log should show a steady stream of IRQs and zero aborts.

---

## Stage 5: EL0 and syscalls — a real privilege boundary

Now drop to user mode. Three pieces: memory user code may run in, a
controlled way down, and a controlled way back up.

**Give EL0 its own region — do not share the kernel's RAM block.** The
working design maps almost everything EL1-only and carves out a small,
dedicated region (its own 4KB pages, via an L2→L3 table split at that
one spot) with `AP_EL1_EL0_RW` and no `UXN`, holding only the user
code and stack. Flipping EL0 permissions onto the same block the
kernel is actively executing from is the configuration that *doesn't*
work reliably; a genuinely separate region does, and it's also the
right architecture — it's your first user/kernel memory separation.

**The way down** is an `eret` with prepared state:

```rust
// kernel/src/tasks.rs (condensed)
pub unsafe fn start() -> ! {
    let ctx = /* task 0's initial Context */;
    asm!(
        "msr sp_el0, {sp}",
        "msr elr_el1, {entry}",
        "msr spsr_el1, {spsr}",
        "eret",
        sp = in(reg) ctx.sp_el0,
        entry = in(reg) ctx.elr_el1,
        spsr = in(reg) ctx.spsr_el1,     // 0 = EL0t, all exceptions unmasked
        options(noreturn),
    );
}
```

`SPSR_EL1 = 0` is the whole mode switch: M[3:0]=0000 selects EL0, and
clear DAIF bits mean the timer tick can preempt user code from the
first instruction.

**The way back up** is `svc`. Pick a convention and freeze it early —
this kernel uses the familiar Linux shape: syscall number in `x8`,
arguments in `x0`–`x3`, return value in `x0`. EL0's `svc` lands in
vector slot 8 ("Synchronous, lower EL AArch64") — but so does any EL0
*fault*, so the slot must check `ESR_EL1`'s exception class first and
only take the syscall path for EC 0x15:

```asm
// slot 8: check EC before committing. NOTE: x9 is userland's live
// register here — save it to a scratch slot BEFORE using it as scratch,
// and have the syscall path recover it. (The one line in this stage
// that is invisibly easy to get wrong.)
sub sp, sp, #16
str x9, [sp]
mrs x9, esr_el1
lsr x9, x9, #26
cmp x9, #0x15
b.eq 3f                    // syscall trampoline
ldr x9, [sp]
add sp, sp, #16
mov x3, #8
b   1f                     // ordinary report-and-halt for EL0 faults
```

The syscall trampoline (`3:`) mirrors the IRQ one — same 272-byte
`Context`-layout frame, including `SP_EL0` — then loads the dispatch
arguments from the saved frame and calls Rust:

```asm
ldr x0, [sp, #64]          // saved x8 -> syscall number
ldp x1, x2, [sp, #0]       // saved x0, x1 -> arg0, arg1
ldp x3, x4, [sp, #16]      // saved x2, x3 -> arg2, arg3
mov x5, sp                 // the frame itself (blocking syscalls, later)
bl  {rust_syscall_handler}
str x0, [sp, #0]           // return value becomes EL0's new x0
// ... restore, eret
```

```rust
// kernel/src/syscall.rs
pub extern "C" fn dispatch(number: u64, arg0: u64, arg1: u64,
                           arg2: u64, arg3: u64,
                           frame: *mut Context) -> u64 {
    match number {
        syscall_abi::PUTC => { console::putc(arg0 as u8); 0 }
        syscall_abi::GET_TICKS => exceptions::ticks(),
        // ...
        _ => u64::MAX,
    }
}
```

One system-register detail: set `SCTLR_EL1.nTWE`/`nTWI` (bits 18/16)
before entering EL0, or user code's own `wfe`/`wfi` traps to EL1
instead of executing.

**Confirm:** a hand-written EL0 stub issues one `svc`, the kernel logs
the number and argument, EL0 resumes, and the timer keeps ticking
*while EL0 runs* (interrupts from EL0 land in slot 9 — point it at the
same resumable trampoline as slot 5, or ticks will silently die the
moment user code starts).

---

## Stage 6: Preemptive multitasking — a struct copy, not a mechanism

Here's the payoff of handing the IRQ trampoline's frame to Rust: task
switching is nothing more than overwriting that frame between save and
restore. The trampoline neither knows nor cares that the frame now
holds a different task's registers — it restores whatever is there and
`eret`s into it.

```rust
// kernel/src/tasks.rs (condensed) — the entire scheduler
static TASKS: [TaskSlot; NUM_TASKS] = /* saved Context per task */;
static CURRENT: AtomicUsize = AtomicUsize::new(0);

fn next_runnable(from: usize) -> usize {
    for offset in 1..=NUM_TASKS {
        let candidate = (from + offset) % NUM_TASKS;
        if state(candidate) == TaskState::Runnable { return candidate; }
    }
    from
}

/// Called from rust_irq_handler on every tick.
pub unsafe fn on_tick(frame: *mut Context) {
    let frame = &mut *frame;
    let current = CURRENT.load(Ordering::Relaxed);
    let next = next_runnable(current);
    if next == current { return; }
    *TASKS[current].0.get() = *frame;    // interrupted task's state out
    *frame = *TASKS[next].0.get();       // next task's state in
    CURRENT.store(next, Ordering::Relaxed);
}
```

Each task's initial `Context` is: `elr_el1` = its entry point,
`sp_el0` = the top of its region (stacks grow down), `spsr_el1` = 0.
Start with two tasks — your real one plus a trivial idle loop — in
separate pages of the EL0 region.

Two working notes: after copying task code into its region, do
D-cache clean + I-cache invalidate over the *whole* region (`dc cvau`
per cache line, then `ic ialluis` + barriers) — instruction and data
caches are not coherent for freshly written code. And make the idle
task a plain busy-spin (`b .` around a `nop`), not `wfe` — `wfe` in an
EL0 task under a real hypervisor can hang in ways an emulator never
shows; the busy-spin costs only idle power you aren't optimizing anyway.

**Confirm:** two tasks each print through a syscall with their own ID;
the output strictly alternates with the tick, for minutes, with zero
aborts in `-d int`. Cross-check the exact syscall count in the trace
against the lines printed.

---

## Stage 7: Userland as real programs — PIE ELF, loaded from disk

Compiled-in task stubs prove the boundary; a real OS loads programs.
The working shortcut for a first loader: read the program file **during
boot services** (UEFI's own FAT driver, `SimpleFileSystem`), stash it
in an allocated region, and hand it to the scheduler after the exit —
no disk driver needed yet.

The load format that works — and removes an entire class of crashes
before you ever hit it — is a **position-independent (PIE) ELF with
self-relocations**. A flat binary linked at base 0 works right up until
the compiler bakes an absolute data pointer into `.rodata` (a
formatting dispatch table, a slice comparison against a literal), which
is then wrong at the real load address. PIE plus a tiny relocation
processor makes every such pointer correct by construction.

Userland target configuration:

```toml
# .cargo/config.toml
[target.aarch64-unknown-none]
rustflags = [
    "-C", "link-arg=-Tshell/linker.ld",
    "-C", "relocation-model=pic",
    "-C", "link-arg=-pie",              # actually produce ET_DYN + .rela.dyn
    "-C", "link-arg=--no-dynamic-linker",
    "-C", "link-arg=-z", "-C", "link-arg=max-page-size=4096",
]
```

(`pic` alone is not enough — without `-pie` the linker resolves
everything to fixed addresses at link time and you're back where you
started. And build userland `--release`: the prebuilt `core` library
contains some non-PIC object code that debug builds' panic machinery
drags into the link.)

The linker script pins the entry point at offset 0 and keeps the
relocation section:

```ld
ENTRY(_start)
SECTIONS
{
    . = 0x0;
    .text : { KEEP(*(.text.start)) *(.text .text.*) }
    .rodata : { *(.rodata .rodata.*) }
    .rela.dyn : { *(.rela.dyn) }
    .dynsym : { *(.dynsym) }
    .dynamic : { *(.dynamic) }
    .data.rel.ro : { *(.data.rel.ro .data.rel.ro.*) }
    .data : { *(.data .data.*) }
    .bss (NOLOAD) : { *(.bss .bss.*) }
}
ASSERT(SIZEOF(.data) == 0, "no loader support for initialized statics yet")
ASSERT(SIZEOF(.bss) == 0, "no loader support for zeroed statics yet")
```

The loader walks `PT_LOAD` program headers (copy `p_filesz` bytes, zero
the `p_memsz` remainder), then applies relocations — for a
no-shared-libraries world, exactly one relocation type exists:

```rust
// kernel/src/loader.rs (condensed)
const R_AARCH64_RELATIVE: u32 = 1027;

unsafe fn apply_relocations(file: &[u8], rela: &SectionHeader, base: u64) {
    let entry_size = 24;   // Elf64_Rela
    let count = rela.sh_size as usize / entry_size;
    for i in 0..count {
        let r: Rela = read_at(file, rela.sh_offset as usize + i * entry_size);
        let r_type = (r.r_info & 0xffff_ffff) as u32;
        assert!(r_type == R_AARCH64_RELATIVE);   // error out on anything else
        let target = base.wrapping_add(r.r_offset);
        let value  = base.wrapping_add(r.r_addend as u64);
        core::ptr::write_unaligned(target as *mut u64, value);
    }
}
```

And a complete userland program is small enough to show whole:

```rust
// hello/src/main.rs
#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[no_mangle]
#[link_section = ".text.start"]     // placed first by the linker script
pub extern "C" fn _start() -> ! { main() }

fn main() -> ! {
    print("Hello from a real userland program!\r\n");
    loop { core::hint::spin_loop(); }
}

fn print(s: &str) {
    for b in s.bytes() { syscall(syscall_abi::PUTC, b as u64); }
}

#[inline(always)]
fn syscall(number: u64, arg0: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!("svc #0",
            inout("x0") arg0 => ret,
            in("x1") 0u64, in("x2") 0u64, in("x3") 0u64,
            in("x8") number,
            options(nostack));
    }
    ret
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! { loop { core::hint::spin_loop(); } }
```

Put the syscall numbers in a shared `#![no_std]` crate of bare
`pub const` values that both kernel and userland depend on — scalar
constants inline as immediates on both sides (no relocation concerns),
and a mismatched number becomes a compile error instead of a runtime
mystery.

**Confirm:** the program's banner prints — output that only exists
because code *loaded from disk* produced it — and, the real acceptance
test, `write!`-formatting a runtime value and comparing a slice against
a literal both work inside it. Those two operations are exactly what a
non-relocated loader breaks.

---

## Stage 8: A disk and a filesystem of your own

To read files *after* boot services (and eventually write them), you
need a block driver and a filesystem.

**virtio-blk over virtio-mmio** is the approachable first block driver.
Attach the disk explicitly (`-device virtio-blk-device` with
`-global virtio-mmio.force-legacy=false` for the modern register
layout) and the driver reduces to: scan the transport's device slots
for a magic value, reset the device, negotiate `VIRTIO_F_VERSION_1`,
set up one virtqueue (descriptor table, available ring, used ring —
plain structs in your own memory), then per request post a 3-descriptor
chain (16-byte header, 512-byte data buffer, 1-byte status), write the
queue-notify doorbell, and poll the used ring with a `dmb` in the
loop. Barriers are for ordering; QEMU's virtio is DMA-coherent, so no
cache maintenance is needed on the buffers. Verify with a read of
sector 0: bytes 510–511 must be `0x55 0xAA` (the MBR signature) — a
value a bug can't produce by accident.

**FAT32, hand-rolled, read-only first.** The format is genuinely small:
sector 0's partition table points at the partition; its BPB (BIOS
Parameter Block) gives sectors-per-cluster, FAT location, and the root
directory's cluster; directories are arrays of 32-byte entries; a
file's content is a linked list of clusters walked through the FAT
(entry N holds the number of the cluster after N; ≥ `0x0FFFFFF8` means
end). Two conventions to build in from day one: a directory entry's
cluster `0` in a `..` entry means "the root directory" (substitute the
real root cluster — for directories only), and stick to 8.3 names
(name your own directories within it; it beats parsing long-filename
entries in a first reader).

A note on heap: after `exit_boot_services` there is no allocator (the
UEFI one died with boot services). Write the FAT32 code — and
everything else post-exit — against fixed-size buffers and statics.
It's less limiting than it sounds and it removes an entire category of
runtime failure.

Expose it as syscalls (`fs_list_dir`, `fs_read_file`, taking pointer +
length pairs) and your userland shell grows `ls` and `cat` over real
storage. Writes, when you're ready, come in the same narrow-first
order this project used: `mkdir`/`rmdir`, then `touch`/`rm`, then
whole-file `write`, then `cp`/`mv` — each one claim-resources-first,
check-everything-before-writing, so a failed operation never leaves
half-applied state.

**Confirm across a reboot:** create a file, kill the VM entirely, boot
fresh, and see the file still there. Only that proves writes reach the
disk rather than a cached shadow of it.

---

## Stage 9: From demo to OS — the mechanisms that compose

Everything past this point reuses machinery you already have, which is
the sign the foundations are right. In the order that worked:

- **Blocking syscalls.** A `read_char` that suspends: if no byte is
  waiting, save the caller's frame into its task slot (the same
  struct-copy as `on_tick`), mark it `Blocked(reason)`, load another
  runnable task into the frame. The tick's handler polls each blocked
  task's wait reason; on success it writes the result into the *saved*
  context's `x0` and marks the task runnable — the task resumes as if
  the syscall had simply returned. One subtlety: have the blocking path
  return the *new* frame's `x0` so the trampoline's unconditional
  return-value write is a no-op, and route input to exactly one
  designated owner task rather than "whichever blocked task polls
  first."
- **`spawn`/`exec`.** A bump allocator carving task regions off the
  top of discovered RAM, plus rebuilding your identity map with one
  more EL0 region — the same table-swap you already trust from boot,
  executed a second time at runtime. The ELF loader from stage 7 is
  already reusable if you split "parse" from "populate."
- **`exit`, `wait`, `kill`, `fg`.** Task slots gain
  `Unused`/`Zombie(status)` states; exit statuses are collected by a
  blocking `wait` built on the same wake-check; keyboard ownership
  becomes a runtime value that reverts to the boot shell whenever its
  owner dies. Add a Ctrl+C intercept at the single point all keyboard
  input flows through, and a stuck foreground task can never brick the
  session.
- **IPC.** Fixed-size messages (64 bytes) copied through the kernel
  into bounded per-task mailboxes; blocking receive is one more wait
  reason. No shared memory — copying is the isolation-friendly
  semantics, and at this size it's free. This is the doorway to the
  microkernel move: servers in userland.

Each of these was verified the same way as everything above: a specific
observable behavior, sustained operation, and a clean `-d int` trace.

---

## Testing techniques worth adopting from day one

- **Script your interactive tests.** QEMU's stdin can be a FIFO
  (`mkfifo pipe; exec 3<>pipe`) so a script can wait for your kernel's
  own "ready" banner in the serial log, then inject keystrokes on a
  timer. Waiting for the banner matters — bytes sent during firmware
  boot get eaten.
- **`-d int -D log` on every run.** QEMU's internal exception trace is
  an independent witness: zero unexpected aborts is your standing
  regression test, and exact SVC/IRQ counts can be cross-checked
  against what your kernel printed. (Count only after a known post-boot
  marker — firmware's own boot-time interrupts pollute whole-run
  totals.)
- **The QEMU monitor is a hardware inspector.** `info qtree` shows what
  devices actually exist, `xp/1xw addr` reads guest physical memory —
  verify a device's register layout *before* writing its driver.
- **Verify state through a second channel.** The MBR signature, a file
  surviving a reboot, a screendump of the framebuffer — each stage's
  confirmation should be a value or behavior your code couldn't
  accidentally fake.

---

## Where to go from here

The path above gets you a preemptive, multi-tasking OS with real
userland programs on QEMU. Taking it to real hardware is its own
journey — different console (a GOP framebuffer, when no UART exists),
different interrupt controller (GICv3, discovered via the same MADT),
genuinely discovered device addresses everywhere — and the same
discover-don't-assume discipline above is precisely what makes it
survivable. That story, including everything that goes differently on
real hardware, is told in the four postmortems:
[boot bring-up](boot-bringup-postmortem.md),
[shell & filesystem](shell-and-filesystem-postmortem.md),
[xHCI keyboard](xhci-keyboard-postmortem.md), and
[USB storage](usb-storage-postmortem.md).

For the complete, working implementation of every stage in this
tutorial — including all the parts condensed here — the
[Ouroboros source](https://github.com/hansolovkarlsson/Ouroboros) is
the reference: `kernel/src/` maps almost one-to-one onto the stages
above, and [`docs/manual.md`](manual.md) covers building and running
it.

//! Devicetree **address translation**: a `reg` address is in the address
//! space of the node's parent bus, not necessarily the CPU's. Each bus node
//! maps its children's addresses into its own parent's space with a `ranges`
//! property, up to the root, whose space is the CPU's physical one (the
//! Devicetree Specification, section 2.3.8).
//!
//! It matters on the Raspberry Pi 4 / Pi 400: every peripheral under `/soc`
//! is described at its VideoCore bus address (the PL011 at `0x7e201000`) and
//! `/soc`'s `ranges` maps `0x7e000000` to the ARM's `0xfe000000`. The first
//! Pi 4 boot took `0x7e201000` as the console's address, which is not a
//! device on that board at all (2026-09-28, docs/testing/testing-pi4.md).
//!
//! Fails closed, as every console discovery here does: an ancestor with no
//! `ranges` (its children are not memory-mapped from the CPU), a range too
//! wide to parse, or an address no range covers gives `None`, never the
//! untranslated address. Depends on `fdt` alone, so a host harness can run it
//! against a real `.dtb`.

use fdt::node::FdtNode;
use fdt::Fdt;

/// Deepest node path searched. Real trees are a handful of levels deep.
const MAX_DEPTH: usize = 16;

/// `addr`, an address in the bus space of `target`'s parent (as `reg()`
/// returns it), translated through every ancestor's `ranges` to a CPU
/// physical address.
pub fn to_cpu(fdt: &Fdt, target: FdtNode, addr: u64) -> Option<u64> {
    // A node is identified by where its `reg` property sits in the blob,
    // which no other node shares.
    let key = target.property("reg")?.value.as_ptr();
    let root = fdt.find_node("/")?;
    let mut path = [None; MAX_DEPTH];
    let depth = find_path(root, key, &mut path, 0)?;
    // path[0] is the root, path[depth] the target. The target's address is
    // in path[depth - 1]'s child space; each bus maps into its parent's.
    let mut addr = addr;
    for i in (1..depth).rev() {
        addr = through_ranges(path[i]?, path[i - 1]?, addr)?;
    }
    Some(addr)
}

fn find_path<'b, 'a>(
    node: FdtNode<'b, 'a>,
    key: *const u8,
    path: &mut [Option<FdtNode<'b, 'a>>; MAX_DEPTH],
    depth: usize,
) -> Option<usize> {
    if depth >= MAX_DEPTH {
        return None;
    }
    path[depth] = Some(node);
    if node.property("reg").is_some_and(|p| p.value.as_ptr() == key) {
        return Some(depth);
    }
    for child in node.children() {
        if let Some(d) = find_path(child, key, path, depth + 1) {
            return Some(d);
        }
    }
    None
}

/// `addr` in `bus`'s child space, mapped into `parent`'s space by `bus`'s
/// `ranges`: entries of (child address, parent address, size), in `bus`'s
/// `#address-cells`, `parent`'s `#address-cells` and `bus`'s `#size-cells`.
/// An empty `ranges` is the identity; an absent one is no mapping at all.
fn through_ranges(bus: FdtNode, parent: FdtNode, addr: u64) -> Option<u64> {
    let ranges = bus.property("ranges")?.value;
    if ranges.is_empty() {
        return Some(addr);
    }
    let child_cells = bus.cell_sizes().address_cells;
    let size_cells = bus.cell_sizes().size_cells;
    let parent_cells = parent.cell_sizes().address_cells;
    let entry = (child_cells + parent_cells + size_cells) * 4;
    if entry == 0 || ranges.len() % entry != 0 {
        return None;
    }
    for e in ranges.chunks_exact(entry) {
        let (child, rest) = e.split_at(child_cells * 4);
        let (parent_addr, size) = rest.split_at(parent_cells * 4);
        let (child, parent_addr, size) = (cells(child)?, cells(parent_addr)?, cells(size)?);
        if addr >= child && addr - child < size {
            return parent_addr.checked_add(addr - child);
        }
    }
    None
}

/// Big-endian cells as one number; more than two (a PCI address) is refused.
fn cells(bytes: &[u8]) -> Option<u64> {
    if bytes.len() > 8 {
        return None;
    }
    Some(bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b)))
}

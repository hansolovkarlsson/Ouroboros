//! `netd`'s network configuration: the `/etc/net` file's format, the
//! values used when there is no file, and the one routing rule (which
//! address a frame for a target is sent to). Pure: bytes in, a value out,
//! no syscalls, so the rules run on the host (`make test`).
//!
//! `docs/roadmap/roadmap-pi-net.md` step 8 (decision D4: a static file
//! now, DHCP a later item of its own).
//!
//! **The file** is lines of `key value`, the two separated by spaces or
//! tabs:
//!
//! ```text
//! # this machine on the bench network
//! address 192.168.1.50
//! mask    255.255.255.0
//! gateway 192.168.1.1
//! dns     192.168.1.1
//! ```
//!
//! `address` and `mask` are required. `gateway` and `dns` may be left
//! out: with no gateway only the machine's own subnet is reached, and
//! with no `dns` no name resolves. A blank line and a line starting with
//! `#` are skipped. **Anything else is refused whole** ([`parse`]): a
//! file that is half understood would give a machine an address its
//! operator did not write.
//!
//! **No file** is [`Config::slirp`]: QEMU user networking's addresses,
//! which is what `netd` held as constants before this crate, so every
//! QEMU rig runs as it did.

#![no_std]

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// The longest file [`parse`] takes. `netd` reads it into a buffer of
/// this size plus one, so a longer file is seen to be longer.
pub const MAX_FILE: usize = 512;

/// An IPv4 address, in the order it is written and sent.
pub type Ip = [u8; 4];

/// A machine's network configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub address: Ip,
    pub mask: Ip,
    /// Where frames for other subnets go. `None`: nowhere.
    pub gateway: Option<Ip>,
    /// The DNS server asked for names. `None`: none is asked.
    pub dns: Option<Ip>,
}

/// Why a file was refused. `line` counts from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Longer than [`MAX_FILE`].
    TooLong,
    /// A line that is not `key value`: one word, or more than two.
    NotKeyValue { line: u32 },
    /// A key that is none of the four.
    UnknownKey { line: u32 },
    /// A key given twice.
    Repeated { line: u32 },
    /// A value that is not four numbers of 0 to 255 with dots between.
    NotAnAddress { line: u32 },
    /// No `address` line.
    NoAddress,
    /// No `mask` line.
    NoMask,
    /// A mask that is not ones then zeros, or is all zeros.
    BadMask,
    /// An address no one machine can have: all zeros, loopback, a group or
    /// reserved address (224 and up), or its subnet's own or broadcast
    /// address.
    BadAddress,
    /// A gateway that is not another machine on the address's subnet.
    BadGateway,
    /// A DNS server that is no one machine's address, or is our own
    /// subnet's own or broadcast address.
    BadDns,
}

const fn to_u32(ip: Ip) -> u32 {
    u32::from_be_bytes(ip)
}

/// Whether `ip` could be one machine's address at all, whatever its
/// subnet.
fn unicast(ip: Ip) -> bool {
    ip != [0; 4] && ip[0] != 0 && ip[0] != 127 && ip[0] < 224
}

// A dotted address by `netaddr`'s rules, the one parser (its doc).
use netaddr::parse_ip;

fn blank(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\r'
}

/// The next word of `line` from `at`, and where it ends.
fn word(line: &[u8], mut at: usize) -> Option<(&[u8], usize)> {
    while at < line.len() && blank(line[at]) {
        at += 1;
    }
    let start = at;
    while at < line.len() && !blank(line[at]) {
        at += 1;
    }
    if at == start {
        None
    } else {
        Some((&line[start..at], at))
    }
}

/// Reads a whole `/etc/net`. Every line must be understood, every value
/// must make sense with the others, or the file is refused and nothing of
/// it is used.
pub fn parse(file: &[u8]) -> Result<Config, Error> {
    if file.len() > MAX_FILE {
        return Err(Error::TooLong);
    }
    let (mut address, mut mask, mut gateway, mut dns) = (None, None, None, None);
    for (index, line) in file.split(|&b| b == b'\n').enumerate() {
        let line_err = index as u32 + 1;
        let Some((key, at)) = word(line, 0) else {
            continue; // blank
        };
        if key[0] == b'#' {
            continue;
        }
        let Some((value, at)) = word(line, at) else {
            return Err(Error::NotKeyValue { line: line_err });
        };
        if word(line, at).is_some() {
            return Err(Error::NotKeyValue { line: line_err });
        }
        let slot = match key {
            b"address" => &mut address,
            b"mask" => &mut mask,
            b"gateway" => &mut gateway,
            b"dns" => &mut dns,
            _ => return Err(Error::UnknownKey { line: line_err }),
        };
        if slot.is_some() {
            return Err(Error::Repeated { line: line_err });
        }
        *slot = Some(parse_ip(value).ok_or(Error::NotAnAddress { line: line_err })?);
    }
    let address = address.ok_or(Error::NoAddress)?;
    let mask = mask.ok_or(Error::NoMask)?;
    let m = to_u32(mask);
    // Ones then zeros: the inverse plus one is a power of two (or zero,
    // for all ones).
    if m == 0 || (!m).wrapping_add(1) & !m != 0 {
        return Err(Error::BadMask);
    }
    let config = Config { address, mask, gateway, dns };
    if !unicast(address) || !config.host_on_subnet(address) {
        return Err(Error::BadAddress);
    }
    if let Some(g) = gateway {
        if g == address || !unicast(g) || !config.on_subnet(g) || !config.host_on_subnet(g) {
            return Err(Error::BadGateway);
        }
    }
    if let Some(d) = dns {
        // On our own subnet it must be a machine there too; elsewhere its
        // subnet is not ours to know.
        if !unicast(d) || d == address || (config.on_subnet(d) && !config.host_on_subnet(d)) {
            return Err(Error::BadDns);
        }
    }
    Ok(config)
}

impl Config {
    /// QEMU user networking (SLIRP): this machine at `10.0.2.<last>` on a
    /// /24, the gateway `10.0.2.2`, the DNS proxy `10.0.2.3`. What `netd`
    /// uses when there is no `/etc/net`; `last` is its to choose (it takes
    /// the MAC address's last byte, 15 for QEMU's default).
    pub const fn slirp(last: u8) -> Config {
        Config { address: [10, 0, 2, last], mask: [255, 255, 255, 0], gateway: Some([10, 0, 2, 2]), dns: Some([10, 0, 2, 3]) }
    }

    /// Whether `ip` is on this machine's subnet.
    pub fn on_subnet(&self, ip: Ip) -> bool {
        let m = to_u32(self.mask);
        to_u32(ip) & m == to_u32(self.address) & m
    }

    /// Whether `ip` is neither its subnet's own address nor its broadcast
    /// address under this mask. A /31 and a /32 have neither.
    fn host_on_subnet(&self, ip: Ip) -> bool {
        let m = to_u32(self.mask);
        let host = to_u32(ip) & !m;
        m >= 0xffff_fffe || (host != 0 && host != !m)
    }

    /// The mask as a count of leading ones, for a log line.
    pub fn prefix(&self) -> u32 {
        to_u32(self.mask).leading_ones()
    }

    /// Where a frame for `target` is sent: `target` itself on this subnet,
    /// the gateway otherwise, and `None` when it is elsewhere and there is
    /// no gateway.
    pub fn next_hop(&self, target: Ip) -> Option<Ip> {
        if self.on_subnet(target) {
            Some(target)
        } else {
            self.gateway
        }
    }
}

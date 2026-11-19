use super::*;

const GOOD: &[u8] = b"# the bench\naddress 192.168.1.50\nmask 255.255.255.0\ngateway 192.168.1.1\ndns 192.168.1.2\n";

fn good() -> Config {
    Config { address: [192, 168, 1, 50], mask: [255, 255, 255, 0], gateway: Some([192, 168, 1, 1]), dns: Some([192, 168, 1, 2]) }
}

#[test]
fn a_whole_file_is_read() {
    assert_eq!(parse(GOOD), Ok(good()));
}

#[test]
fn order_spacing_blank_lines_and_crlf_do_not_matter() {
    let file = b"\r\n\tdns\t192.168.1.2\r\n\n  mask   255.255.255.0  \r\n# x y z w\n#\naddress 192.168.1.50\ngateway 192.168.1.1";
    assert_eq!(parse(file), Ok(good()));
}

#[test]
fn gateway_and_dns_may_be_left_out() {
    let c = parse(b"address 10.1.2.3\nmask 255.255.0.0\n").unwrap();
    assert_eq!((c.gateway, c.dns), (None, None));
    assert_eq!(c.prefix(), 16);
}

#[test]
fn address_and_mask_may_not() {
    assert_eq!(parse(b""), Err(Error::NoAddress));
    assert_eq!(parse(b"mask 255.255.255.0\n"), Err(Error::NoAddress));
    assert_eq!(parse(b"address 10.1.2.3\n"), Err(Error::NoMask));
}

#[test]
fn a_line_not_understood_refuses_the_file_and_names_the_line() {
    assert_eq!(parse(b"address 10.1.2.3\nmask\n"), Err(Error::NotKeyValue { line: 2 }));
    assert_eq!(parse(b"address 10.1.2.3 extra\n"), Err(Error::NotKeyValue { line: 1 }));
    assert_eq!(parse(b"\n\nrouter 10.1.2.1\n"), Err(Error::UnknownKey { line: 3 }));
    assert_eq!(parse(b"Address 10.1.2.3\n"), Err(Error::UnknownKey { line: 1 }));
    assert_eq!(parse(b"address 10.1.2.3\naddress 10.1.2.3\n"), Err(Error::Repeated { line: 2 }));
    // A comment is a whole line; one after a value is a third word.
    assert_eq!(parse(b"address 10.1.2.3 # mine\n"), Err(Error::NotKeyValue { line: 1 }));
}

/// What an address is, is `netaddr`'s (its tests); a value that is not
/// one refuses the file.
#[test]
fn a_value_that_is_not_an_address_refuses_the_file() {
    assert_eq!(parse(b"address 10.1.2\nmask 255.0.0.0\n"), Err(Error::NotAnAddress { line: 1 }));
}

#[test]
fn a_mask_is_ones_then_zeros() {
    for bad in ["0.0.0.0", "255.0.255.0", "255.255.255.1", "0.255.255.255", "254.255.0.0"] {
        let file = std::format!("address 10.1.2.3\nmask {bad}\n");
        assert_eq!(parse(file.as_bytes()), Err(Error::BadMask), "{bad}");
    }
    for (ok, prefix) in [("128.0.0.0", 1), ("255.255.255.0", 24), ("255.255.255.252", 30), ("255.255.255.254", 31), ("255.255.255.255", 32)] {
        let file = std::format!("address 10.1.2.5\nmask {ok}\n");
        assert_eq!(parse(file.as_bytes()).map(|c| c.prefix()), Ok(prefix), "{ok}");
    }
}

#[test]
fn an_address_no_machine_can_have_is_refused() {
    for bad in ["0.0.0.0", "0.1.2.3", "127.0.0.1", "224.0.0.1", "255.255.255.255", "10.1.2.0", "10.1.2.255"] {
        let file = std::format!("address {bad}\nmask 255.255.255.0\n");
        assert_eq!(parse(file.as_bytes()), Err(Error::BadAddress), "{bad}");
    }
    // The same last bytes are a machine's under a wider mask.
    assert!(parse(b"address 10.1.2.0\nmask 255.255.0.0\n").is_ok());
    assert!(parse(b"address 10.1.2.255\nmask 255.255.0.0\n").is_ok());
}

#[test]
fn a_gateway_is_another_machine_on_the_subnet() {
    for bad in ["192.168.1.50", "192.168.2.1", "192.168.1.0", "192.168.1.255", "0.0.0.0"] {
        let file = std::format!("address 192.168.1.50\nmask 255.255.255.0\ngateway {bad}\n");
        assert_eq!(parse(file.as_bytes()), Err(Error::BadGateway), "{bad}");
    }
}

#[test]
fn a_dns_server_is_some_other_machine_anywhere() {
    assert!(parse(b"address 192.168.1.50\nmask 255.255.255.0\ndns 8.8.8.8\n").is_ok());
    // Off our subnet, a last byte of 0 or 255 may be a machine's.
    assert!(parse(b"address 192.168.1.50\nmask 255.255.255.0\ndns 10.9.8.255\n").is_ok());
    for bad in ["192.168.1.50", "0.0.0.0", "224.0.0.251", "127.0.0.1", "192.168.1.255", "192.168.1.0"] {
        let file = std::format!("address 192.168.1.50\nmask 255.255.255.0\ndns {bad}\n");
        assert_eq!(parse(file.as_bytes()), Err(Error::BadDns), "{bad}");
    }
}

#[test]
fn a_file_longer_than_the_limit_is_refused_whole() {
    let mut file = std::vec::Vec::from(GOOD);
    file.resize(MAX_FILE, b'\n');
    assert_eq!(parse(&file), Ok(good()));
    file.push(b'\n');
    assert_eq!(parse(&file), Err(Error::TooLong));
}

#[test]
fn the_next_hop_is_the_target_on_the_subnet_and_the_gateway_off_it() {
    let c = good();
    assert_eq!(c.next_hop([192, 168, 1, 7]), Some([192, 168, 1, 7]));
    assert_eq!(c.next_hop([192, 168, 2, 7]), Some([192, 168, 1, 1]));
    assert_eq!(c.next_hop([8, 8, 8, 8]), Some([192, 168, 1, 1]));
    let alone = Config { gateway: None, ..c };
    assert_eq!(alone.next_hop([192, 168, 1, 7]), Some([192, 168, 1, 7]));
    assert_eq!(alone.next_hop([8, 8, 8, 8]), None);
    // A /16 reaches what a /24 would route.
    let wide = parse(b"address 10.1.2.3\nmask 255.255.0.0\ngateway 10.1.0.1\n").unwrap();
    assert_eq!(wide.next_hop([10, 1, 200, 9]), Some([10, 1, 200, 9]));
    assert_eq!(wide.next_hop([10, 2, 0, 9]), Some([10, 1, 0, 1]));
}

#[test]
fn no_file_is_what_netd_held_as_constants() {
    let c = Config::slirp(15);
    assert_eq!(c, Config { address: [10, 0, 2, 15], mask: [255, 255, 255, 0], gateway: Some([10, 0, 2, 2]), dns: Some([10, 0, 2, 3]) });
    // The old rule, word for word: 10.0.2.x is direct, all else the gateway.
    assert_eq!(c.next_hop([10, 0, 2, 99]), Some([10, 0, 2, 99]));
    assert_eq!(c.next_hop([10, 0, 3, 1]), Some([10, 0, 2, 2]));
    assert_eq!(c.next_hop([93, 184, 216, 34]), Some([10, 0, 2, 2]));
    // And it is a file's worth: the same values written out are accepted.
    assert_eq!(parse(b"address 10.0.2.15\nmask 255.255.255.0\ngateway 10.0.2.2\ndns 10.0.2.3\n"), Ok(c));
}

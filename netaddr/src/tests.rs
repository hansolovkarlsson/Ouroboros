use super::*;

#[test]
fn an_address_is_four_numbers_and_nothing_else() {
    for ok in [&b"0.0.0.0"[..], b"255.255.255.255", b"10.0.2.15", b"010.000.002.015"] {
        assert!(parse_ip(ok).is_some(), "{:?}", core::str::from_utf8(ok));
    }
    assert_eq!(parse_ip(b"010.000.002.015"), Some([10, 0, 2, 15]));
    for bad in [&b""[..], b"1.2.3", b"1.2.3.4.5", b"1.2.3.", b".1.2.3", b"1..2.3", b"256.1.1.1", b"1.2.3.4 ", b"1.2.3.-4", b"1.2.3.4x", b"0001.2.3.4", b"1,2,3,4", b"1.2.3.+4"] {
        assert_eq!(parse_ip(bad), None, "{:?}", core::str::from_utf8(bad));
    }
}

/// netd's old parser took any digits and wrapped: this was `1.0.0.1`.
#[test]
fn a_part_too_long_to_fit_is_refused_and_not_wrapped() {
    assert_eq!(parse_ip(b"18446744073709551617.0.0.1"), None);
    assert_eq!(parse_ip(b"1.2.3.18446744073709551620"), None);
}

#[test]
fn addresses_round_trip() {
    for ip in [[0u8, 0, 0, 0], [10, 0, 2, 15], [192, 168, 1, 254], [255, 255, 255, 255], [1, 20, 100, 9]] {
        let mut buf = [0u8; IP_TEXT_MAX];
        let n = format_ip(&ip, &mut buf);
        assert_eq!(parse_ip(&buf[..n]), Some(ip), "for {ip:?}");
    }
    let mut buf = [0u8; IP_TEXT_MAX];
    let n = format_ip(&[10, 0, 2, 15], &mut buf);
    assert_eq!(&buf[..n], b"10.0.2.15");
}

#[test]
fn a_decimal_is_digits_alone() {
    assert_eq!(parse_dec(b"0"), Some(0));
    assert_eq!(parse_dec(b"80"), Some(80));
    assert_eq!(parse_dec(b"18446744073709551615"), Some(u64::MAX));
    for bad in [&b""[..], b" 80", b"80 ", b"+80", b"-1", b"8x0"] {
        assert_eq!(parse_dec(bad), None, "{:?}", core::str::from_utf8(bad));
    }
}

/// The review of #490: netd's reader wrapped, so this port was 80.
#[test]
fn a_decimal_past_u64_is_refused_and_not_wrapped() {
    assert_eq!(parse_dec(b"18446744073709551616"), None);
    assert_eq!(parse_dec(b"18446744073709551696"), None);
}

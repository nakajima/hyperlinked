use super::*;

#[test]
fn split_type_defaults_to_local_domain() {
    let (service_type, domain) =
        split_service_type_and_domain("_hyperlinked._tcp").expect("service type should parse");
    assert_eq!(service_type, "_hyperlinked._tcp");
    assert_eq!(domain, "local.");
}

#[test]
fn split_type_with_explicit_domain() {
    let (service_type, domain) = split_service_type_and_domain("_hyperlinked._tcp.local.")
        .expect("service type should parse");
    assert_eq!(service_type, "_hyperlinked._tcp");
    assert_eq!(domain, "local.");
}

#[test]
fn split_type_accepts_non_local_domain() {
    let (service_type, domain) = split_service_type_and_domain("_hyperlinked._tcp.lan.example.")
        .expect("service type should parse");
    assert_eq!(service_type, "_hyperlinked._tcp");
    assert_eq!(domain, "lan.example.");
}

#[test]
fn split_type_rejects_invalid_service_type() {
    let err = split_service_type_and_domain("hyperlinked-tcp")
        .expect_err("invalid service type should fail");
    assert!(err.contains("invalid mDNS service type"));
}

#[test]
fn default_service_name_is_not_empty() {
    assert!(!MdnsOptions::default_service_name().trim().is_empty());
}

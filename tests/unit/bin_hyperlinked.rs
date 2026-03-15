use super::parse_auto_schema_sync_on_serve;

#[test]
fn auto_schema_sync_defaults_to_enabled() {
    assert!(parse_auto_schema_sync_on_serve(None));
}

#[test]
fn auto_schema_sync_disable_values_are_honored() {
    for value in ["0", "false", "no", "off", " FALSE ", "Off"] {
        assert!(!parse_auto_schema_sync_on_serve(Some(value)));
    }
}

#[test]
fn auto_schema_sync_non_disable_values_enable_sync() {
    for value in ["1", "true", "yes", "on", "custom", ""] {
        assert!(parse_auto_schema_sync_on_serve(Some(value)));
    }
}

use super::*;

#[test]
fn only_root_hyperlinks_enqueue_sublink_discovery() {
    assert!(should_enqueue_sublink_discovery(ROOT_DISCOVERY_DEPTH));
    assert!(!should_enqueue_sublink_discovery(
        crate::app::models::hyperlink::DISCOVERED_DISCOVERY_DEPTH
    ));
}

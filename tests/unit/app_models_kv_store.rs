use super::*;
use crate::test_support;

#[tokio::test]
async fn set_get_delete_round_trips_values() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;

    set(&connection, "settings.example", "true")
        .await
        .expect("key should save");
    let loaded = get(&connection, "settings.example")
        .await
        .expect("key should load");
    assert_eq!(loaded.as_deref(), Some("true"));

    delete(&connection, "settings.example")
        .await
        .expect("key should delete");
    let loaded = get(&connection, "settings.example")
        .await
        .expect("key should load after delete");
    assert_eq!(loaded, None);
}

#[tokio::test]
async fn get_many_returns_existing_entries() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;

    set(&connection, "settings.one", "1")
        .await
        .expect("first key should save");
    set(&connection, "settings.two", "0")
        .await
        .expect("second key should save");

    let values = get_many(
        &connection,
        &["settings.one", "settings.two", "settings.three"],
    )
    .await
    .expect("keys should load");

    assert_eq!(values.get("settings.one").map(String::as_str), Some("1"));
    assert_eq!(values.get("settings.two").map(String::as_str), Some("0"));
    assert!(!values.contains_key("settings.three"));
}

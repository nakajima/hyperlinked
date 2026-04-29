use super::*;
use crate::test_support;

#[tokio::test]
async fn load_defaults_when_keys_are_missing() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    let loaded = load(&connection).await.expect("settings should load");
    assert_eq!(loaded, ArtifactCollectionSettings::default());
}

#[tokio::test]
async fn save_normalizes_dependent_flags() {
    let connection = test_support::new_memory_connection().await;
    test_support::initialize_hyperlinks_schema(&connection).await;
    let saved = save(
        &connection,
        ArtifactCollectionSettings {
            collect_source: false,
            collect_screenshots: true,
            collect_screenshot_dark: true,
            collect_og: true,
            collect_readability: true,
        },
    )
    .await
    .expect("settings should save");

    assert_eq!(
        saved,
        ArtifactCollectionSettings {
            collect_source: false,
            collect_screenshots: false,
            collect_screenshot_dark: false,
            collect_og: false,
            collect_readability: false,
        }
    );

    let loaded = load(&connection).await.expect("settings should load");
    assert_eq!(loaded, saved);
}

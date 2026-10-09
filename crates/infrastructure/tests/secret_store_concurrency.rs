use infrastructure::secrets::{FileSecretStore, SecretStore};
#[tokio::test]
async fn independent_instances_preserve_concurrent_secret_updates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.json");
    let mut tasks = vec![];
    for i in 0..30 {
        let store = FileSecretStore::new(path.clone(), [7; 32]);
        tasks.push(tokio::spawn(async move {
            store
                .put(&format!("key-{i}"), &format!("value-{i}"))
                .await
                .unwrap()
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let store = FileSecretStore::new(&path, [7; 32]);
    for i in 0..30 {
        assert_eq!(
            store.get(&format!("key-{i}")).await.unwrap().unwrap(),
            format!("value-{i}")
        );
    }
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(!raw.contains("value-"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

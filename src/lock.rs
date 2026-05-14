use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

/// Per-scope compile lock. Matches CE's LockManager.acquire(compileDir).
/// Different scopes run concurrently; same-scope waits.
#[derive(Default)]
pub struct ScopeLocks {
    inner: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl ScopeLocks {
    pub async fn get(&self, scope: &str) -> Arc<Mutex<()>> {
        let mut map = self.inner.lock().await;
        map.entry(scope.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

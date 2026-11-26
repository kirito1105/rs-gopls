use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use crate::workspace::Workspace;

pub(crate) struct TestProject {
    pub(crate) root: PathBuf,
}

impl TestProject {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "rs-gopls-symbols-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }

    pub(crate) fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    pub(crate) fn workspace(&self, files: &[&str], includes_tests: bool) -> Workspace {
        let package = json!({
            "Dir": self.root,
            "ImportPath": "example.test/sample",
            "Name": "sample",
            "GoFiles": files,
        });
        Workspace::from_test_listing(
            std::slice::from_ref(&self.root),
            &package.to_string(),
            includes_tests,
        )
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

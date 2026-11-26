use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    Uri,
    notification::{DidChangeTextDocument, DidOpenTextDocument, Notification as _},
};
use serde_json::{Value, json};

use crate::workspace::Workspace;

pub(crate) struct TestSession {
    server: Connection,
    client: Connection,
}

impl TestSession {
    pub(crate) fn new() -> Self {
        let (server, client) = Connection::memory();
        Self { server, client }
    }

    pub(crate) fn server(&self) -> &Connection {
        &self.server
    }

    pub(crate) fn request(
        &self,
        id: impl Into<RequestId>,
        method: &str,
        params: Value,
        dispatch: impl FnOnce(&Connection, Request) -> anyhow::Result<()>,
    ) {
        dispatch(&self.server, Request::new(id.into(), method.into(), params)).unwrap();
    }

    pub(crate) fn notify(
        &self,
        method: &str,
        params: Value,
        dispatch: impl FnOnce(&Connection, Notification) -> anyhow::Result<()>,
    ) {
        dispatch(&self.server, Notification::new(method.into(), params)).unwrap();
    }

    pub(crate) fn open_go_document(
        &self,
        uri: &Uri,
        text: &str,
        dispatch: impl FnOnce(&Connection, Notification) -> anyhow::Result<()>,
    ) {
        self.notify(
            DidOpenTextDocument::METHOD,
            json!({
                "textDocument": {"uri": uri, "languageId": "go", "version": 1, "text": text},
            }),
            dispatch,
        );
    }

    pub(crate) fn replace_document(
        &self,
        uri: &Uri,
        version: i32,
        text: &str,
        dispatch: impl FnOnce(&Connection, Notification) -> anyhow::Result<()>,
    ) {
        self.notify(
            DidChangeTextDocument::METHOD,
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": text}],
            }),
            dispatch,
        );
    }

    pub(crate) fn response(&self, id: impl Into<RequestId>) -> Response {
        let Message::Response(response) = self
            .client
            .receiver
            .try_recv()
            .expect("request must have a response")
        else {
            panic!("expected an LSP response");
        };
        assert_eq!(response.id, id.into());
        response
    }

    pub(crate) fn assert_no_messages(&self) {
        assert!(self.client.receiver.is_empty(), "unexpected LSP messages");
    }
}

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

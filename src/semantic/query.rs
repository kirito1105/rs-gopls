use std::convert::Infallible;

use anyhow::Result;

use super::Engine;
use crate::workspace::WorkspaceLoad;

pub(crate) enum QueryProgress<T, Task = Infallible> {
    Ready(T),
    Pending(WorkspaceLoad),
    Searching(Task),
}

pub(crate) trait SemanticTask<T> {
    fn ready(&self, _engine: &Engine) -> bool {
        true
    }

    fn discard(&mut self, _code: lsp_server::ErrorCode) {}

    fn advance(&mut self, engine: &mut Engine) -> Result<bool>;
    fn finish(self) -> T;
}

impl<T> SemanticTask<T> for Infallible {
    fn advance(&mut self, _engine: &mut Engine) -> Result<bool> {
        match *self {}
    }

    fn finish(self) -> T {
        match self {}
    }
}

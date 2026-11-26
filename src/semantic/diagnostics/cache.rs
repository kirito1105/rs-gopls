use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use super::{DiagnosticReport, DocumentSnapshot};
use crate::workspace::path_key;

const CAPACITY: usize = 256;

#[derive(Default)]
pub(crate) struct DiagnosticsCache {
    reports: HashMap<PathBuf, DiagnosticReport>,
    order: VecDeque<PathBuf>,
}

impl DiagnosticsCache {
    #[allow(dead_code, reason = "snapshot lookup for code actions")]
    pub(crate) fn get(
        &self,
        document: &DocumentSnapshot<'_>,
        graph_generation: u64,
        includes_tests: bool,
    ) -> Option<&DiagnosticReport> {
        self.report(&path_key(document.path())).filter(|report| {
            report
                .snapshot
                .matches(document, graph_generation, includes_tests)
        })
    }

    pub(super) fn report(&self, key: &Path) -> Option<&DiagnosticReport> {
        self.reports.get(key)
    }

    pub(crate) fn insert(&mut self, report: DiagnosticReport) {
        let path = &report.snapshot.path_key;
        if !self.reports.contains_key(path) {
            if self.order.len() == CAPACITY
                && let Some(oldest) = self.order.pop_front()
            {
                self.reports.remove(&oldest);
            }
            self.order.push_back(path.clone());
        }
        self.reports.insert(path.clone(), report);
    }

    #[allow(dead_code, reason = "document lifecycle entry point")]
    pub(crate) fn invalidate_document(&mut self, path_key: &Path) {
        if self.reports.remove(path_key).is_some() {
            self.order.retain(|key| key != path_key);
        }
    }

    #[allow(dead_code, reason = "workspace lifecycle entry point")]
    pub(crate) fn clear(&mut self) {
        self.reports.clear();
        self.order.clear();
    }
}

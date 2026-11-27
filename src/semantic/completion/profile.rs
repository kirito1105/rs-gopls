use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

use lsp_types::Position;
use serde::Serialize;

use super::sdk::{InventoryStats, unix_ms};

#[derive(Clone)]
pub(crate) struct Trace(Rc<RefCell<Profile>>);

struct Profile {
    output: &'static Path,
    started: Instant,
    stats: Stats,
    reads: HashMap<PathBuf, usize>,
}

#[derive(Serialize)]
struct Stats {
    path: PathBuf,
    position: Position,
    started_unix_ms: u64,
    total_ms: f64,
    phases_ms: BTreeMap<&'static str, f64>,
    sdk_state: &'static str,
    sdk_at_import_search: &'static str,
    sdk: Option<InventoryStats>,
    import_search_unix_ms: Option<u64>,
    prefilter_reads: usize,
    project_prefilter_reads: usize,
    disk_reads: usize,
    second_reads: usize,
    third_or_later_reads: usize,
    indexed_source_reads: usize,
    text_handoffs: usize,
    full_parses: usize,
    declaration_parses: usize,
    type_location_hits: usize,
    expected_weight_hits: usize,
    peak_sources: usize,
    peak_source_bytes: usize,
    request_cache_released: bool,
    outcome: &'static str,
    slow_indexes: Vec<IndexSample>,
}

#[derive(Serialize)]
struct IndexSample {
    file: PathBuf,
    bytes: usize,
    declaration_parse: bool,
    elapsed_ms: f64,
}

pub(in crate::semantic) struct Phase(Trace, &'static str, Instant);

impl Trace {
    pub(crate) fn start(path: &Path, position: Position) -> Option<Self> {
        static OUTPUT: OnceLock<Option<PathBuf>> = OnceLock::new();
        let output = OUTPUT.get_or_init(|| {
            std::env::var_os("RS_GOPLS_COMPLETION_PROFILE")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
        });
        Some(Self(Rc::new(RefCell::new(Profile {
            output: output.as_deref()?,
            started: Instant::now(),
            stats: Stats {
                path: path.to_owned(),
                position,
                started_unix_ms: unix_ms(),
                total_ms: 0.0,
                phases_ms: BTreeMap::new(),
                sdk_state: "not used",
                sdk_at_import_search: "not used",
                sdk: None,
                import_search_unix_ms: None,
                prefilter_reads: 0,
                project_prefilter_reads: 0,
                disk_reads: 0,
                second_reads: 0,
                third_or_later_reads: 0,
                indexed_source_reads: 0,
                text_handoffs: 0,
                full_parses: 0,
                declaration_parses: 0,
                type_location_hits: 0,
                expected_weight_hits: 0,
                peak_sources: 0,
                peak_source_bytes: 0,
                request_cache_released: false,
                outcome: "discarded",
                slow_indexes: Vec::new(),
            },
            reads: HashMap::new(),
        }))))
    }

    pub(in crate::semantic) fn phase(&self, name: &'static str) -> Phase {
        Phase(self.clone(), name, Instant::now())
    }

    pub(super) fn sdk(&self, state: &'static str, stats: Option<InventoryStats>) {
        let mut profile = self.0.borrow_mut();
        if profile.stats.import_search_unix_ms.is_none() {
            profile.stats.import_search_unix_ms = Some(unix_ms());
            profile.stats.sdk_at_import_search = state;
        }
        profile.stats.sdk_state = state;
        profile.stats.sdk = stats;
    }

    pub(in crate::semantic) fn read(
        &self,
        key: &Path,
        prefilter: bool,
        project: bool,
        indexed: bool,
    ) {
        let mut profile = self.0.borrow_mut();
        let reads = profile.reads.entry(key.to_owned()).or_default();
        *reads += 1;
        let reads = *reads;
        let stats = &mut profile.stats;
        stats.disk_reads += 1;
        stats.prefilter_reads += usize::from(prefilter);
        stats.project_prefilter_reads += usize::from(prefilter && project);
        stats.indexed_source_reads += usize::from(indexed);
        stats.second_reads += usize::from(reads == 2);
        stats.third_or_later_reads += usize::from(reads >= 3);
    }

    pub(in crate::semantic) fn parse(&self) {
        self.0.borrow_mut().stats.full_parses += 1;
    }

    pub(in crate::semantic) fn declaration_parse(&self) {
        self.0.borrow_mut().stats.declaration_parses += 1;
    }

    pub(in crate::semantic) fn index_time(
        &self,
        file: &Path,
        bytes: usize,
        declaration_parse: bool,
        started: Instant,
    ) {
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        if elapsed_ms >= 2.0 {
            self.0.borrow_mut().stats.slow_indexes.push(IndexSample {
                file: file.to_owned(),
                bytes,
                declaration_parse,
                elapsed_ms,
            });
        }
    }

    pub(in crate::semantic) fn text_handoff(&self) {
        self.0.borrow_mut().stats.text_handoffs += 1;
    }

    pub(in crate::semantic) fn type_location_hit(&self) {
        self.0.borrow_mut().stats.type_location_hits += 1;
    }

    pub(super) fn expected_weight_hit(&self) {
        self.0.borrow_mut().stats.expected_weight_hits += 1;
    }

    pub(in crate::semantic) fn cache_size(&self, count: usize, bytes: usize) {
        let mut profile = self.0.borrow_mut();
        profile.stats.peak_sources = profile.stats.peak_sources.max(count);
        profile.stats.peak_source_bytes = profile.stats.peak_source_bytes.max(bytes);
    }

    pub(in crate::semantic) fn cache_released(&self) {
        self.0.borrow_mut().stats.request_cache_released = true;
    }

    pub(crate) fn outcome(&self, outcome: &'static str) {
        self.0.borrow_mut().stats.outcome = outcome;
    }

    pub(crate) fn error(&self, code: lsp_server::ErrorCode) {
        use lsp_server::ErrorCode;
        self.outcome(match code {
            ErrorCode::RequestCanceled => "cancelled",
            ErrorCode::ContentModified => "stale",
            _ => "failed",
        });
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        let mut profile = self.0.0.borrow_mut();
        *profile.stats.phases_ms.entry(self.1).or_default() +=
            self.2.elapsed().as_secs_f64() * 1000.0;
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        self.stats.total_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        if let Ok(mut line) = serde_json::to_vec(&self.stats)
            && let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.output)
        {
            line.push(b'\n');
            let _ = file.write_all(&line);
        }
    }
}

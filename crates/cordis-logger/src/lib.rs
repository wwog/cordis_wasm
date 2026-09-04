//! Structured logging, bounded history, and effect-owned exporters.
//!
//! This is deliberately a parallel application logging service, not a `tracing-subscriber`
//! implementation. Runtime adapters may emit the same event to both systems, while installing a
//! subscriber that feeds records back into [`Logger`] would risk recursion and duplicate delivery.

use cordis_core::{CordisError, Disposer, EffectScope, FiberId};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static NEXT_EXPORTER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogRecord {
    pub sequence: u64,
    pub level: LogLevel,
    pub target: Arc<str>,
    pub message: Arc<str>,
    pub fiber: Option<FiberId>,
}

pub trait LogExporter: Send + Sync + 'static {
    fn export(&self, record: &LogRecord);
}

struct LoggerState {
    next_sequence: u64,
    capacity: usize,
    records: VecDeque<LogRecord>,
    levels: BTreeMap<String, LogLevel>,
    exporters: BTreeMap<u64, Arc<dyn LogExporter>>,
}

/// Cloneable structured logger with fixed-capacity history.
#[derive(Clone)]
pub struct Logger {
    state: Arc<Mutex<LoggerState>>,
}

impl std::fmt::Debug for Logger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        formatter
            .debug_struct("Logger")
            .field("capacity", &state.capacity)
            .field("records", &state.records.len())
            .field("exporters", &state.exporters.len())
            .finish()
    }
}

impl Logger {
    pub fn new(capacity: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(LoggerState {
                next_sequence: 1,
                capacity,
                records: VecDeque::with_capacity(capacity),
                levels: BTreeMap::new(),
                exporters: BTreeMap::new(),
            })),
        }
    }

    pub fn set_level(&self, target: impl Into<String>, minimum: LogLevel) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .levels
            .insert(target.into(), minimum);
    }

    /// Registers an exporter whose removal is owned by `scope`.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] when the scope is disposing.
    pub fn register_exporter(
        &self,
        exporter: Arc<dyn LogExporter>,
        scope: &EffectScope,
    ) -> Result<u64, CordisError> {
        let id = NEXT_EXPORTER_ID.fetch_add(1, Ordering::Relaxed);
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .exporters
            .insert(id, exporter);
        let state = self.state.clone();
        if let Err(error) = scope.defer(Disposer::infallible(move || async move {
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .exporters
                .remove(&id);
        })) {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .exporters
                .remove(&id);
            return Err(error);
        }
        Ok(id)
    }

    pub fn log(
        &self,
        level: LogLevel,
        target: impl Into<Arc<str>>,
        message: impl Into<Arc<str>>,
        fiber: Option<FiberId>,
    ) {
        let target = target.into();
        let (record, exporters) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let minimum = state
                .levels
                .iter()
                .filter(|(prefix, _)| target.starts_with(prefix.as_str()))
                .max_by_key(|(prefix, _)| prefix.len())
                .map_or(LogLevel::Info, |(_, level)| *level);
            if level < minimum {
                return;
            }
            let record = LogRecord {
                sequence: state.next_sequence,
                level,
                target,
                message: message.into(),
                fiber,
            };
            state.next_sequence = state.next_sequence.saturating_add(1);
            if state.capacity > 0 {
                if state.records.len() == state.capacity {
                    state.records.pop_front();
                }
                state.records.push_back(record.clone());
            }
            (
                record,
                state.exporters.values().cloned().collect::<Vec<_>>(),
            )
        };
        for exporter in exporters {
            exporter.export(&record);
        }
    }

    pub fn records(&self) -> Vec<LogRecord> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .cloned()
            .collect()
    }
}

/// Plain stderr exporter kept outside `cordis-core`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConsoleExporter;

impl LogExporter for ConsoleExporter {
    fn export(&self, record: &LogRecord) {
        let fiber = record
            .fiber
            .map_or_else(|| "-".to_owned(), |fiber| fiber.to_string());
        let now = chrono::Local::now().format("%Y/%m/%d %H:%M:%S%.3f");
        eprintln!(
            "[{now}] [{:?}] [{}] [fiber={fiber}] {}",
            record.level, record.target, record.message
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cordis_core::EffectGuard;

    #[derive(Default)]
    struct Capture(Mutex<Vec<LogRecord>>);

    impl LogExporter for Capture {
        fn export(&self, record: &LogRecord) {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(record.clone());
        }
    }

    #[tokio::test]
    async fn buffer_is_bounded_and_exporter_is_effect_owned() {
        let logger = Logger::new(2);
        let capture = Arc::new(Capture::default());
        let (owner, scope) = EffectGuard::new("logger");
        logger.register_exporter(capture.clone(), &scope).unwrap();
        logger.log(LogLevel::Info, "app", "one", None);
        logger.log(LogLevel::Info, "app", "two", None);
        logger.log(LogLevel::Warn, "app", "three", None);
        assert_eq!(logger.records().len(), 2);
        assert_eq!(capture.0.lock().unwrap().len(), 3);
        owner.dispose().await.unwrap();
        logger.log(LogLevel::Error, "app", "four", None);
        assert_eq!(capture.0.lock().unwrap().len(), 3);
    }

    #[test]
    fn longest_target_filter_wins() {
        let logger = Logger::new(4);
        logger.set_level("app", LogLevel::Warn);
        logger.set_level("app.db", LogLevel::Debug);
        logger.log(LogLevel::Info, "app.http", "hidden", None);
        logger.log(LogLevel::Debug, "app.db.query", "visible", None);
        assert_eq!(logger.records().len(), 1);
    }
}

//! Collect narrow library timing traces in the eval JSON report.

use std::sync::Mutex;

use crate::Timings;

const TARGET: &str = "tinymemory_eval_timing";

struct TimingLogger(Mutex<Vec<(String, f64)>>);

static LOGGER: TimingLogger = TimingLogger(Mutex::new(Vec::new()));

impl log::Log for TimingLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == TARGET
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = record.args().to_string();
        let Some((stage, value)) = line.split_once('=') else {
            return;
        };
        let Ok(ms) = value.parse::<f64>() else {
            return;
        };
        if let Ok(mut samples) = self.0.lock() {
            samples.push((stage.replace('_', " "), ms));
        }
    }

    fn flush(&self) {}
}

pub(crate) fn install() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Trace);
    }
}

pub(crate) fn drain(timings: &mut Timings) {
    if let Ok(mut samples) = LOGGER.0.lock() {
        for (stage, ms) in samples.drain(..) {
            timings.add(&stage, ms);
        }
    }
}

//! NIN workflow engine: DAG scheduling of jobs/steps with matrix expansion,
//! automatic parallelism, retries, timeouts, resource limits and result collection.

mod engine;
mod report;
mod sink;

pub use engine::{graph_text, plan, run, Options};
pub use report::{JobReport, Report, StepReport};
pub use sink::{ConsoleSink, PrefixSink};

//! nin-sched: task scheduler for `nin schedule ...`.
//!
//! * [`trigger`]: trigger parsing / printing / next-run math (naive local time, DST-insensitive)
//! * [`store`]: `schedules.toml` persistence (atomic writes), ids, due-task selection
//! * [`runner`]: in-process runner (daemon) with injectable clock
//! * [`os`]: Windows Task Scheduler integration (XML import through `schtasks.exe`)

mod error;
pub mod os;
pub mod runner;
pub mod store;
pub mod trigger;

pub use error::SchedError;
pub use os::{
    os_query, os_register, os_run_now, os_unregister, schtasks_create_args, schtasks_delete_args,
    schtasks_query_args, schtasks_run_args, task_xml, task_xml_utf16, DEFAULT_TASK_PREFIX,
};
pub use runner::Runner;
pub use store::{due_tasks, due_tasks_with, make_id, ScheduledTask, Store};
pub use trigger::{next_run, parse_duration, parse_trigger, Trigger};

pub use chrono::Weekday;
pub type Result<T> = std::result::Result<T, SchedError>;

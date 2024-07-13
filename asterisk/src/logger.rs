use std::{ffi::c_int, ptr::null};

use log::{Level, Log};

use asterisk_sys::bindings::ast_log_safe;
use asterisk_sys::bindings::ast_verb_sys_level;

pub struct AsteriskLogger;

impl AsteriskLogger {
    fn asterisk_log_evel(level: Level) -> i32 {
        // https://github.com/asterisk/asterisk/blob/8b5ddfee5ef903582fbc2b51b3083d4c885aecce/include/asterisk/logger.h#L244-L319
        match level {
            Level::Error => 4,
            Level::Warn => 3,
            Level::Info => 2, // notice
            Level::Debug => 0,
            Level::Trace => 1,
        }
    }

    fn verbosity_at_least(level: Level) -> bool {
        let raw_level = unsafe { ast_verb_sys_level };
        Self::asterisk_log_evel(level) <= raw_level
    }
}

impl Log for AsteriskLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        Self::verbosity_at_least(metadata.level())
    }

    fn log(&self, record: &log::Record) {
        let formatted = format!("{}", record.args());
        if cfg!(debug_assertions) {
            if record.target().contains("chan_discord") || record.target().contains("asterisk") {
                println!("chan_discord: {formatted}");
            }
        }

        unsafe {
            ast_log_safe(
                Self::asterisk_log_evel(record.level()),
                null(),
                match record.line() {
                    Some(line) => line as i32,
                    None => -1,
                },
                null(),
                c"%.*s".as_ptr().cast(),
                formatted.len() as c_int,
                formatted.as_ptr(),
            );
        }
    }

    fn flush(&self) {}
}

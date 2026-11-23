#![no_main]
//! JDBC bridge: frames from the Java host (routing, JSON answers, host errors, Arrow IPC).

use libfuzzer_sys::fuzz_target;

fuzz_target!(
    init: ferrodb_jdbc::fuzzing::tolerate_guarded_arrow_panics(),
    |data: &[u8]| {
        ferrodb_jdbc::fuzzing::host_stream(data);
    }
);

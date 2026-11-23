#![no_main]
//! JDBC bridge: error positions read from vendor messages.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ferrodb_jdbc::fuzzing::error_text(data);
});

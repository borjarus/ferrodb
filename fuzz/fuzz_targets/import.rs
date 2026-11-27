#![no_main]
//! Import readers (CSV, TSV, JSON, JSON Lines): byte 0 = format and options.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ferrodb_data::fuzzing::import_bytes(data);
});

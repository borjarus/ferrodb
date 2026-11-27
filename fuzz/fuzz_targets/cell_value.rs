#![no_main]
//! Grid cell text and JSON converted to every column type.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ferrodb_data::fuzzing::cell_text(&String::from_utf8_lossy(data));
});

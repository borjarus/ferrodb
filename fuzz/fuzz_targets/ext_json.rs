#![no_main]
//! JSON / Extended JSON -> BSON and every conversion of the result.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ferrodb_data::fuzzing::ext_json(data);
});

#![no_main]
//! MongoDB shell values: parse, print, parse again.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ferrodb_lang::fuzzing::mql_value(&String::from_utf8_lossy(data));
});

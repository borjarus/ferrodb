#![no_main]
//! Every language service entry point (all SQL dialects, MQL, CQL, SQL++, PartiQL, Redis): byte 0 = language, bytes 1..3 = cursor.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ferrodb_lang::fuzzing::language_service_bytes(data);
});

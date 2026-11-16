#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(init: mdl_fuzz::warm_up(), |data: &[u8]| mdl_fuzz::layouts(data));

#![no_main]

use libfuzzer_sys::fuzz_target;
use nanoctl_update::verify_for_current_target;

fuzz_target!(|data: &[u8]| {
    let _ = verify_for_current_target(
        data,
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "1.0.0",
        1_800_000_000,
    );
});

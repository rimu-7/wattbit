//! Read-only smoke tests against the real hardware of the machine running
//! them. They never write to the SMC. Run with:
//!
//! ```sh
//! cargo test --test hardware -- --ignored --nocapture
//! ```

use berry::smc::{keys, Smc, SmcError};

#[test]
#[ignore = "touches real hardware"]
fn smc_known_keys_are_readable_or_absent() {
    let smc = Smc::open().expect("AppleSMC should be reachable on every Mac");
    for descriptor in keys::KNOWN_KEYS {
        match smc.read(descriptor.key) {
            Ok(value) => println!(
                "{} type={:?} size={} value=[{}]  ({})",
                descriptor.key,
                value.info.data_type.to_string(),
                value.info.size,
                value.hex(),
                descriptor.purpose
            ),
            Err(SmcError::KeyNotFound(_)) => {
                println!("{} absent  ({})", descriptor.key, descriptor.purpose)
            }
            Err(other) => panic!("unexpected error reading {}: {other}", descriptor.key),
        }
    }
}

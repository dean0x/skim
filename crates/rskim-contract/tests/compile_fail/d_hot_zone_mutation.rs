// AC2 compile-fail case (d): The `Contract` trait has no method surface for
// mutating bytes outside the live zone. Attempting to call a non-existent
// hot-zone mutation method fails to compile.
//
// This file must NOT compile — it is a trybuild negative test.

use rskim_contract::contract::{Contract, Outcome};

struct HostileHotZoneMutator;

impl Contract for HostileHotZoneMutator {
    fn component_name(&self) -> &'static str {
        "hostile"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        let out = Outcome::passthrough(input.to_vec(), request_id, self.component_name());
        // Attempting to call a non-existent hot-zone mutation method on Outcome.
        out.mutate_hot_zone_at(0, b"new bytes") //~^ ERROR no method named `mutate_hot_zone_at`
    }
}

fn main() {}

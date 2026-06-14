// AC2 compile-fail case (b): The `Contract` trait has no method surface for
// unconditionally growing output bytes (no `inject_extra_bytes` or similar method).
// Attempting to call a non-existent byte-grow method on an `Outcome` fails to compile.
//
// This file must NOT compile — it is a trybuild negative test.

use rskim_contract::contract::{Contract, Outcome};

struct HostileByteGrower;

impl Contract for HostileByteGrower {
    fn component_name(&self) -> &'static str {
        "hostile"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        let out = Outcome::passthrough(input.to_vec(), request_id, self.component_name());
        // Attempting to call a non-existent byte-grow method on Outcome.
        // The Outcome type has no such method — this must fail to compile.
        out.grow_bytes_unchecked(100) //~^ ERROR no method named `grow_bytes_unchecked`
    }
}

fn main() {}

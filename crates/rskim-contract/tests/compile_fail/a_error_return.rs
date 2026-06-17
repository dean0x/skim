// AC2 compile-fail case (a): The `Contract::transform` method cannot return an error.
// Attempting to return `Result<Outcome, _>` from `transform` fails because the trait
// requires returning `Outcome` directly.
//
// This file must NOT compile — it is a trybuild negative test.

use rskim_contract::contract::{Contract, Outcome};

struct HostileErrorReturner;

impl Contract for HostileErrorReturner {
    fn component_name(&self) -> &'static str {
        "hostile"
    }

    // Attempting to return Result instead of Outcome is a type mismatch.
    fn transform(&self, input: &[u8], request_id: &str) -> Result<Outcome, String> {
        //~^ ERROR mismatched types
        Ok(Outcome::passthrough(
            input.to_vec(),
            request_id,
            self.component_name(),
        ))
    }
}

fn main() {}

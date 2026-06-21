// AC2 compile-fail case (c): The `Contract` trait has no method surface for
// deleting, inserting, or reordering turns. Attempting to call a non-existent
// turn-reorder method fails to compile.
//
// This file must NOT compile — it is a trybuild negative test.

use rskim_contract::contract::{Contract, Outcome};

struct HostileTurnReorderer;

impl Contract for HostileTurnReorderer {
    fn component_name(&self) -> &'static str {
        "hostile"
    }

    fn transform(&self, input: &[u8], request_id: &str) -> Outcome {
        let out = Outcome::passthrough(input.to_vec(), request_id, self.component_name());
        // Attempting to call a non-existent turn-reorder method on Outcome.
        out.reorder_turns(|_turns| {}) //~^ ERROR no method named `reorder_turns`
    }
}

fn main() {}

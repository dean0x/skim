//! Module-level doc comment (KEEP)
//! This describes the module (KEEP)

// FIXTURE: Rust file with various comment types
// TESTS: Minimal mode comment stripping

// This is a regular line comment (STRIP)
/* This is a regular block comment (STRIP) */

/// Function doc comment (KEEP)
/// Multi-line doc comment (KEEP)
pub fn add(a: i32, b: i32) -> i32 {
    // This comment is inside a function body (KEEP)
    let result = a + b; // inline comment in body (KEEP)
    result
}

// Regular comment between items (STRIP)

/// Struct doc comment (KEEP)
pub struct Calculator {
    /// Field doc comment (KEEP)
    value: i32,
}

/* Regular block comment at module level (STRIP) */

/// Impl doc comment (KEEP)
impl Calculator {
    /// Constructor doc (KEEP)
    pub fn new(value: i32) -> Self {
        // body comment (KEEP)
        Self { value }
    }

    /// Add method doc (KEEP)
    pub fn add(&self, x: i32) -> i32 {
        self.value + x
    }
}

//! Another inner doc comment (KEEP)

// Regular comment (STRIP)




// Test blank line normalization: 4+ blank lines above (normalize to 2)

pub const VERSION: &str = "1.0.0";

// FIXTURE: Swift file with various comment types
// TESTS: Minimal mode comment stripping

import Foundation

// This is a standalone comment (STRIP)

/* Block comment at module level (STRIP) */

/// Doc comment for add function (KEEP)
/// - Parameters:
///   - a: First number
///   - b: Second number
/// - Returns: Sum of a and b
func add(_ a: Int, _ b: Int) -> Int {
    // This comment is inside a function body (KEEP)
    let result = a + b // inline comment in body (KEEP)
    return result
}

// Regular comment between declarations (STRIP)

/**
 * Calculator class documentation (KEEP)
 */
class Calculator {
    private let value: Int

    /// Initializer doc (KEEP)
    init(value: Int) {
        self.value = value
    }

    /// Add method doc (KEEP)
    func add(_ x: Int) -> Int {
        // body comment (KEEP)
        return value + x
    }
}

/* Standalone block comment (STRIP) */




// Test blank line normalization: 4+ blank lines above (normalize to 2)

let VERSION = "1.0.0"

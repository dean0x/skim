// FIXTURE: C++ file with various comment types
// TESTS: Minimal mode comment stripping

#include <string>

// This is a standalone comment (STRIP)

/* Block comment at module level (STRIP) */

/**
 * A simple calculator class.
 * Supports basic arithmetic operations.
 */
class Calculator {
public:
    /**
     * Construct a new Calculator.
     * @param initial Starting value
     */
    Calculator(int initial) : value_(initial) {}

    /// Add a value (KEEP - Doxygen)
    int add(int x) {
        // body comment (KEEP)
        value_ += x;
        return value_;
    }

private:
    int value_;
};

// Regular comment between declarations (STRIP)

/// Greet a person by name (KEEP - Doxygen)
std::string greet(const std::string& name) {
    /* body block comment (KEEP) */
    return "Hello, " + name + "!";
}

/* Standalone block comment (STRIP) */

int global_var = 42;




// Test blank line normalization: 4+ blank lines above (normalize to 2)

void cleanup() {
    // body cleanup (KEEP)
}

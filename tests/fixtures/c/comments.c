// FIXTURE: C file with various comment types
// TESTS: Minimal mode comment stripping

#include <stdio.h>

// This is a standalone comment (STRIP)

/* Block comment at module level (STRIP) */

/**
 * Adds two integers together.
 * @param a First number
 * @param b Second number
 * @return Sum of a and b
 */
int add(int a, int b) {
    // This comment is inside a function body (KEEP)
    int result = a + b; // inline comment in body (KEEP)
    return result;
}

// Regular comment between functions (STRIP)

/// Doxygen single-line doc comment (KEEP)
void greet(const char* name) {
    /* body block comment (KEEP) */
    printf("Hello, %s!\n", name);
}

/* Standalone block comment (STRIP) */

struct Point {
    int x;
    int y;
};




// Test blank line normalization: 4+ blank lines above (normalize to 2)

int global_var = 42;

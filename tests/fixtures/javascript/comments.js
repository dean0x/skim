// FIXTURE: JavaScript file with various comment types
// TESTS: Minimal mode comment stripping

// This is a regular single-line comment (STRIP)
/* This is a regular block comment (STRIP) */

/**
 * This is a JSDoc comment (KEEP)
 * @param {number} x - first number
 * @param {number} y - second number
 * @returns {number} the sum
 */
function add(x, y) {
    // This comment is inside a function body (KEEP)
    const result = x + y; // inline comment in body (KEEP)
    return result;
}

// Another regular comment (STRIP)

/**
 * A documented class (KEEP)
 */
class Calculator {
    /** Constructor doc (KEEP) */
    constructor(value) {
        this.value = value;
    }

    /**
     * Add method doc (KEEP)
     */
    add(x) {
        // body comment (KEEP)
        return this.value + x;
    }
}

/* Regular block comment at module level (STRIP) */




// Test blank line normalization: 4+ blank lines above (normalize to 2)

const VERSION = "1.0.0";

// FIXTURE: Kotlin file with various comment types
// TESTS: Minimal mode comment stripping

package com.example

// This is a standalone comment (STRIP)

/* Block comment at module level (STRIP) */

/**
 * KDoc comment for add function (KEEP)
 * @param a First number
 * @param b Second number
 * @return Sum of a and b
 */
fun add(a: Int, b: Int): Int {
    // This comment is inside a function body (KEEP)
    val result = a + b // inline comment in body (KEEP)
    return result
}

// Regular comment between declarations (STRIP)

/**
 * Calculator class documentation (KEEP)
 */
class Calculator(private val value: Int) {
    /** Constructor-related doc (KEEP) */
    fun add(x: Int): Int {
        // body comment (KEEP)
        return value + x
    }
}

/* Standalone block comment (STRIP) */




// Test blank line normalization: 4+ blank lines above (normalize to 2)

const val VERSION = "1.0.0"

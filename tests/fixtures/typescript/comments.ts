// FIXTURE: TypeScript file with various comment types
// TESTS: Minimal mode comment stripping

// This is a regular single-line comment (STRIP)
/* This is a regular block comment (STRIP) */

/**
 * This is a JSDoc comment (KEEP)
 * @param x - first number
 * @param y - second number
 * @returns the sum
 */
export function add(x: number, y: number): number {
    // This comment is inside a function body (KEEP)
    const result = x + y; // inline comment in body (KEEP)
    return result;
}

// Another regular comment (STRIP)
// Multiple lines of regular comments (STRIP)

/**
 * A documented class (KEEP)
 */
export class Calculator {
    private value: number;

    /** Constructor doc (KEEP) */
    constructor(value: number) {
        this.value = value;
    }

    /**
     * Add method doc (KEEP)
     */
    add(x: number): number {
        // body comment (KEEP)
        return this.value + x;
    }
}

/* Regular block comment at module level (STRIP) */

export interface Config {
    name: string;
    value: number;
}




// Test blank line normalization: there are 4+ blank lines above (normalize to 2)

export const VERSION = "1.0.0";

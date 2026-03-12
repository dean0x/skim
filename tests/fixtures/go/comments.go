// FIXTURE: Go file with various comment types
// TESTS: Minimal mode comment stripping

package main

// This is a standalone comment not adjacent to a declaration (STRIP)

/* This is a standalone block comment (STRIP) */

// Add adds two numbers together.
// This is a Go doc comment adjacent to a declaration (KEEP).
func Add(a int, b int) int {
	// This comment is inside a function body (KEEP)
	result := a + b // inline comment in body (KEEP)
	return result
}

// Regular comment not adjacent to declaration (STRIP)

// Calculator is a simple calculator.
// Multi-line Go doc comment (KEEP).
type Calculator struct {
	// Value field comment inside struct (STRIP - not a function body)
	value int
}

/* Block comment at module level not adjacent to declaration (STRIP) */

// NewCalculator creates a new Calculator.
func NewCalculator(value int) *Calculator {
	// body comment (KEEP)
	return &Calculator{value: value}
}

// Add adds to the calculator value.
func (c *Calculator) Add(x int) int {
	return c.value + x
}

// Standalone comment (STRIP)




// Test blank line normalization: 4+ blank lines above (normalize to 2)

var Version = "1.0.0"

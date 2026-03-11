// FIXTURE: Java file with various comment types
// TESTS: Minimal mode comment stripping

// This is a regular single-line comment (STRIP)
/* This is a regular block comment (STRIP) */

/**
 * This is a Javadoc comment (KEEP)
 * @author test
 */
public class Comments {
    private int value;

    /**
     * Constructor Javadoc (KEEP)
     * @param value initial value
     */
    public Comments(int value) {
        // This comment is inside a method body (KEEP)
        this.value = value; // inline comment in body (KEEP)
    }

    /**
     * Add method Javadoc (KEEP)
     * @param a first number
     * @param b second number
     * @return the sum
     */
    public int add(int a, int b) {
        // body comment (KEEP)
        return a + b;
    }

    // Regular comment inside class but outside method (STRIP)

    public String greet(String name) {
        return "Hello, " + name + "!";
    }
}

/* Regular block comment at top level (STRIP) */




// Test blank line normalization: 4+ blank lines above (normalize to 2)

// Regular comment (STRIP)

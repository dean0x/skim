// FIXTURE: C# file with various comment types
// TESTS: Minimal mode comment stripping

using System;

// This is a standalone comment (STRIP)

/* Block comment at module level (STRIP) */

/// <summary>
/// XML doc comment for Add method (KEEP)
/// </summary>
/// <param name="a">First number</param>
/// <param name="b">Second number</param>
/// <returns>Sum of a and b</returns>
public static int Add(int a, int b)
{
    // This comment is inside a method body (KEEP)
    int result = a + b; // inline comment in body (KEEP)
    return result;
}

// Regular comment between members (STRIP)

/** Block doc comment for Calculator class (KEEP) */
public class Calculator
{
    private int _value;

    /// <summary>Constructor doc (KEEP)</summary>
    public Calculator(int value)
    {
        _value = value;
    }

    /// <summary>Add method doc (KEEP)</summary>
    public int Add(int x)
    {
        // body comment (KEEP)
        return _value + x;
    }
}

/* Standalone block comment at namespace level (STRIP) */




// Test blank line normalization: 4+ blank lines above (normalize to 2)

public const int VERSION = 1;

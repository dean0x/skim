#!/usr/bin/env python3
# FIXTURE: Python file with various comment types
# TESTS: Minimal mode comment stripping

# This is a regular comment at module level (STRIP)

# Another regular comment (STRIP)

def add(x: int, y: int) -> int:
    """Add two numbers together.

    This is a docstring and should be preserved (KEEP).

    Args:
        x: first number
        y: second number

    Returns:
        The sum of x and y
    """
    # This comment is inside a function body (KEEP)
    result = x + y  # inline comment in body (KEEP)
    return result

# Regular comment between functions (STRIP)

class Calculator:
    """A simple calculator class (KEEP)."""

    def __init__(self, value: int) -> None:
        """Initialize calculator (KEEP)."""
        self.value = value

    def add(self, x: int) -> int:
        """Add to stored value (KEEP)."""
        # body comment (KEEP)
        return self.value + x

# Regular module-level comment (STRIP)




# Test blank line normalization: 4+ blank lines above (normalize to 2)

VERSION = "1.0.0"

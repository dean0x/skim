# FIXTURE: Ruby file with various comment types
# TESTS: Minimal mode comment stripping

require 'json'

# This is a standalone comment (STRIP)

# Another standalone comment (STRIP)

# Add two numbers together.
# This is a RDoc-style doc comment adjacent to method (KEEP).
def add(a, b)
  # This comment is inside a method body (KEEP)
  result = a + b # inline comment in body (KEEP)
  result
end

# Regular comment between methods (STRIP)

# Calculator class documentation (KEEP).
class Calculator
  # Initialize the calculator (KEEP).
  def initialize(value)
    @value = value
  end

  # Add to stored value (KEEP).
  def add(x)
    # body comment (KEEP)
    @value + x
  end
end

# Standalone comment at module level (STRIP)




# Test blank line normalization: 4+ blank lines above (normalize to 2)

VERSION = "1.0.0"

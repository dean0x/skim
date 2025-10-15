"""
FIXTURE: Simple Python functions
TESTS: Basic function signature extraction
"""

def calculate_sum(a: int, b: int) -> int:
    """Calculate sum of two numbers"""
    result = a + b
    return result

def greet_user(name: str) -> str:
    """Greet a user by name"""
    message = f"Hello, {name}!"
    return message

class Calculator:
    def add(self, x: int, y: int) -> int:
        return x + y

    def multiply(self, x: int, y: int) -> int:
        return x * y

import os
import sys
from typing import Optional, List

class UserError(Exception):
    """Custom error for user operations."""
    pass

class User:
    """Represents a user in the system."""
    def __init__(self, name: str, email: str):
        self.name = name
        self.email = email

    def validate(self) -> bool:
        return bool(self.name and self.email)

    def to_dict(self) -> dict:
        return {"name": self.name, "email": self.email}

def create_user(name: str, email: str) -> User:
    """Create a new user with validation."""
    if not name:
        raise UserError("Name is required")
    if not email:
        raise UserError("Email is required")
    return User(name, email)

def find_user(users: List[User], name: str) -> Optional[User]:
    """Find a user by name."""
    for user in users:
        if user.name == name:
            return user
    return None

MAX_USERS = 1000
DEFAULT_EMAIL = "unknown@example.com"

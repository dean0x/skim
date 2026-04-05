"""Utility functions for the authentication service."""

from datetime import datetime, timedelta
from typing import Optional

import hashlib
import hmac


def hash_password(password: str, salt: str) -> str:
    """Hash a password with the given salt using SHA-256."""
    return hashlib.sha256(f"{salt}{password}".encode()).hexdigest()


def verify_password(password: str, salt: str, expected_hash: str) -> bool:
    """Verify a password against its expected hash."""
    return hmac.compare_digest(hash_password(password, salt), expected_hash)


class TokenGenerator:
    """Generates and validates time-limited authentication tokens."""

    def __init__(self, secret: str, ttl_hours: int = 24):
        self.secret = secret
        self.ttl = timedelta(hours=ttl_hours)

    def generate(self, user_id: str) -> str:
        """Generate a new token for the given user."""
        expires = datetime.utcnow() + self.ttl
        payload = f"{user_id}:{expires.isoformat()}"
        signature = hmac.new(
            self.secret.encode(), payload.encode(), hashlib.sha256
        ).hexdigest()
        return f"{payload}:{signature}"

    def validate(self, token: str) -> Optional[str]:
        """Validate a token and return the user_id if valid."""
        parts = token.split(":")
        if len(parts) != 3:
            return None
        user_id, expires_str, signature = parts
        payload = f"{user_id}:{expires_str}"
        expected = hmac.new(
            self.secret.encode(), payload.encode(), hashlib.sha256
        ).hexdigest()
        if not hmac.compare_digest(signature, expected):
            return None
        expires = datetime.fromisoformat(expires_str)
        if datetime.utcnow() > expires:
            return None
        return user_id

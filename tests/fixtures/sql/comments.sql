-- FIXTURE: SQL file with various comment types
-- TESTS: Minimal mode comment stripping

-- This is a standalone line comment (STRIP)

/* This is a standalone block comment (STRIP) */

-- Create users table
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,     -- inline comment (KEEP in body)
    email TEXT UNIQUE NOT NULL
);

/* Block comment between statements (STRIP) */

-- Create orders table
CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    user_id INTEGER REFERENCES users(id),
    total DECIMAL(10, 2)
);

-- Standalone comment (STRIP)

SELECT u.name, COUNT(o.id) AS order_count
FROM users u
LEFT JOIN orders o ON u.id = o.user_id
GROUP BY u.name;




-- Test blank line normalization: 4+ blank lines above (normalize to 2)

CREATE INDEX idx_users_email ON users(email);

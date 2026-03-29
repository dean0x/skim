-- FIXTURE: Simple SQL file
-- TESTS: Basic SQL statement structure extraction

CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT UNIQUE NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE orders (
    id INTEGER PRIMARY KEY,
    user_id INTEGER REFERENCES users(id),
    total DECIMAL(10, 2),
    status TEXT DEFAULT 'pending'
);

SELECT u.name, u.email, COUNT(o.id) AS order_count
FROM users u
LEFT JOIN orders o ON u.id = o.user_id
WHERE u.created_at > '2024-01-01'
GROUP BY u.name, u.email
HAVING COUNT(o.id) > 0
ORDER BY order_count DESC;

CREATE INDEX idx_users_email ON users(email);

INSERT INTO users (name, email) VALUES ('Alice', 'alice@example.com');

UPDATE users SET name = 'Bob' WHERE id = 1;

DELETE FROM orders WHERE status = 'cancelled';

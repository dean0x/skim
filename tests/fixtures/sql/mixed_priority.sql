-- FIXTURE: SQL file with mixed priority items
-- TESTS: Truncation priority testing

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

CREATE INDEX idx_users_email ON users(email);
CREATE INDEX idx_orders_user_id ON orders(user_id);

CREATE VIEW active_users AS
SELECT u.id, u.name, u.email
FROM users u
WHERE EXISTS (SELECT 1 FROM orders o WHERE o.user_id = u.id);

SELECT u.name, COUNT(o.id) AS order_count
FROM users u
LEFT JOIN orders o ON u.id = o.user_id
GROUP BY u.name
HAVING COUNT(o.id) > 0
ORDER BY order_count DESC;

INSERT INTO users (name, email) VALUES ('Alice', 'alice@example.com');

UPDATE users SET name = 'Bob' WHERE id = 1;

DELETE FROM orders WHERE status = 'cancelled';

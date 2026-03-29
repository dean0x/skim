-- FIXTURE: SQL views and aggregate queries
-- TESTS: View extraction

CREATE VIEW active_users AS
SELECT id, name, email
FROM users
WHERE status = 'active';

CREATE VIEW order_summary AS
SELECT
    u.name,
    COUNT(o.id) AS total_orders,
    SUM(o.total) AS total_spent
FROM users u
JOIN orders o ON u.id = o.user_id
GROUP BY u.name;

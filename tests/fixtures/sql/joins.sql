-- FIXTURE: SQL join patterns
-- TESTS: Complex query structure

SELECT
    u.name AS user_name,
    o.id AS order_id,
    p.name AS product_name,
    oi.quantity,
    oi.price
FROM users u
INNER JOIN orders o ON u.id = o.user_id
INNER JOIN order_items oi ON o.id = oi.order_id
INNER JOIN products p ON oi.product_id = p.id
WHERE o.status = 'completed'
ORDER BY u.name, o.id;

SELECT
    d.name AS department,
    COUNT(e.id) AS employee_count,
    AVG(e.salary) AS avg_salary
FROM departments d
LEFT JOIN employees e ON d.id = e.department_id
GROUP BY d.name
HAVING COUNT(e.id) > 5;

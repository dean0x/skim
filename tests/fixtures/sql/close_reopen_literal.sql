-- Clean SQL before literals
SELECT 'first
literal content' || '
second
literal content' AS combined
FROM dual;
-- Clean line after

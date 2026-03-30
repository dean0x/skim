-- FIXTURE: SQL schema definitions
-- TESTS: DDL structure extraction

CREATE TABLE products (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    price DECIMAL(10, 2) NOT NULL,
    category TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE order_items (
    id INTEGER PRIMARY KEY,
    order_id INTEGER REFERENCES orders(id),
    product_id INTEGER REFERENCES products(id),
    quantity INTEGER NOT NULL DEFAULT 1,
    price DECIMAL(10, 2) NOT NULL
);

CREATE INDEX idx_products_category ON products(category);
CREATE INDEX idx_order_items_order ON order_items(order_id);

ALTER TABLE products ADD COLUMN stock INTEGER DEFAULT 0;

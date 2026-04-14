-- A migration file with various DSQL incompatibilities.
-- Used by fixture_sample_migration test to verify each is caught.

-- Should pass: valid DSQL
CREATE TABLE tenants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL
);

-- SERIAL type (error)
CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    name TEXT
);

-- FOREIGN KEY (error)
CREATE TABLE orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID,
    FOREIGN KEY (user_id) REFERENCES users(id)
);

-- JSON column (error)
CREATE TABLE events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    payload JSON
);

-- TRUNCATE (error)
TRUNCATE TABLE events;

-- TEMP TABLE (error)
CREATE TEMP TABLE scratch (val INT);

-- Array type (error)
CREATE TABLE tags (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    labels TEXT[]
);

-- Sync CREATE INDEX — should require ASYNC (error)
CREATE INDEX idx_orders ON orders(user_id);

-- Valid DML (should NOT error)
INSERT INTO tenants (id, name) VALUES (gen_random_uuid(), 'Acme Corp');
SELECT * FROM tenants WHERE name = 'Acme Corp';
DELETE FROM events WHERE id = gen_random_uuid();

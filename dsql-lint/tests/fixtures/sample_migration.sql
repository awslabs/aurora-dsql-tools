-- A migration file with various DSQL incompatibilities

-- Should pass: valid DSQL
CREATE TABLE tenants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL
);

-- E003: SERIAL type
CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    name TEXT
);

-- E001: FOREIGN KEY
CREATE TABLE orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(255) NOT NULL,
    user_id UUID,
    FOREIGN KEY (user_id) REFERENCES users(id)
);

-- E006: JSON column
CREATE TABLE events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(255) NOT NULL,
    payload JSON
);

-- E007: TRUNCATE
TRUNCATE TABLE events;

-- E008: TEMP TABLE
CREATE TEMP TABLE scratch (val INT);

-- E009: Array type
CREATE TABLE tags (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id VARCHAR(255) NOT NULL,
    labels TEXT[]
);

-- W001: Missing tenant_id
CREATE TABLE settings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    key VARCHAR(255),
    value TEXT
);

-- Valid DML (should not error)
INSERT INTO tenants (id, tenant_id, name) VALUES (gen_random_uuid(), 'acme', 'Acme Corp');
SELECT * FROM tenants WHERE tenant_id = 'acme';
DELETE FROM events WHERE tenant_id = 'acme';

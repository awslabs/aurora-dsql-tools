-- V7: Add category_id column to users table
-- Tests ALTER TABLE ADD COLUMN for relationship (without FK constraint)
-- A foreign key can be added separately with NOT VALID and validated asynchronously.

ALTER TABLE flyway_test_users ADD COLUMN category_id UUID;

INSERT INTO async_idx_users (id, email)
SELECT g, 'user' || g || '@example.com' FROM generate_series(1, 500) g;

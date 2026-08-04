-- The plugin must wait for this build to finish before the migration completes.
CREATE UNIQUE INDEX ASYNC idx_async_idx_users_email ON async_idx_users(email);

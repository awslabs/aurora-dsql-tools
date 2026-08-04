-- Cannot succeed: the data contains duplicates. The migration must fail, not pass silently.
CREATE UNIQUE INDEX ASYNC idx_async_idx_dupes_email ON async_idx_dupes(email);

SELECT EXISTS (
    SELECT 1 FROM _sqlx_migrations WHERE version = 20261102000001 AND success
) AS "applied!";

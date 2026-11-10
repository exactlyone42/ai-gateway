SELECT NOT EXISTS (SELECT 1 FROM _sqlx_migrations WHERE NOT success) AS "clean!";

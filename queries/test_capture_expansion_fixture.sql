BEGIN;
CREATE TABLE capture_expansion_fixture (payload text NOT NULL);
INSERT INTO capture_expansion_fixture
SELECT repeat(md5(value::text), 8192)
FROM generate_series(1, 100) AS value;
COMMIT;

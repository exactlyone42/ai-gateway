BEGIN;
WITH updated AS (
    INSERT INTO capture_erasure_floor (singleton, install, sequence, tip)
    VALUES (true, :'install'::uuid, :'sequence'::bigint, decode(:'tip', 'hex'))
    ON CONFLICT (singleton) DO UPDATE
    SET install = EXCLUDED.install, sequence = EXCLUDED.sequence, tip = EXCLUDED.tip
    WHERE capture_erasure_floor.install = EXCLUDED.install
      AND (capture_erasure_floor.sequence < EXCLUDED.sequence
           OR (capture_erasure_floor.sequence = EXCLUDED.sequence AND capture_erasure_floor.tip = EXCLUDED.tip))
    RETURNING singleton
)
SELECT 1 / count(*)::integer FROM updated;
DROP SCHEMA capture_restore_incomplete;
COMMIT;

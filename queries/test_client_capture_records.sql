SELECT COALESCE(json_agg(record ORDER BY created_at, capture_id), '[]'::json)
FROM (
    SELECT capture_id, state, created_at
    FROM capture_records
    WHERE user_id = :'user'::uuid
) AS record;

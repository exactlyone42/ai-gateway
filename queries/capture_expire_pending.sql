SELECT EXISTS (
    SELECT 1 FROM capture_records
    WHERE state = 'deleting' OR (expires_at <= now() AND state <> 'deleted')
) AS "pending!";

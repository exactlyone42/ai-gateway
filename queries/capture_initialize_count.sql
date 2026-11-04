SELECT (SELECT count(*) FROM capture_records) + (SELECT count(*) FROM capture_master_key_usage) + (SELECT count(*) FROM capture_erasure_marks) AS "count!"

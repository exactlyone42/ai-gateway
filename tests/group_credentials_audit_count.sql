SELECT count(*) AS "count!" FROM audit_logs WHERE request_id = $1 AND action = $2;

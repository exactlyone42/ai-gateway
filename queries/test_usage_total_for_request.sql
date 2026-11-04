SELECT COALESCE(sum(input_tokens + output_tokens), 0)::bigint AS "tokens!" FROM usage_records WHERE gateway_request_id = $1

CREATE OR REPLACE FUNCTION valid_request_parameter_snapshot(parameters JSONB) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT jsonb_typeof(parameters) = 'object'
       AND octet_length(parameters::text) <= 4096
       AND parameters - ARRAY[
           'schema_version', 'operation', 'model', 'max_output_tokens',
           'temperature', 'top_p', 'top_k', 'stream', 'parallel_tool_calls',
           'message_count', 'content_part_count', 'text_chars', 'image_count',
           'tool_count', 'has_system', 'has_structured_output', 'reasoning'
       ] = '{}'::jsonb
       AND parameters->>'schema_version' = '1'
       AND parameters->>'operation' IN (
           'anthropic_messages', 'anthropic_count_tokens',
           'openai_responses', 'openai_chat_completions',
           'gemini_generate_content'
       )
       AND (parameters->'model' IS NULL OR (
           jsonb_typeof(parameters->'model') = 'string'
           AND parameters->>'model' ~ '^[A-Za-z0-9._:/-]{1,255}$'
       ))
       AND (parameters->'max_output_tokens' IS NULL
            OR jsonb_typeof(parameters->'max_output_tokens') = 'number')
       AND (parameters->'temperature' IS NULL
            OR jsonb_typeof(parameters->'temperature') = 'number')
       AND (parameters->'top_p' IS NULL OR jsonb_typeof(parameters->'top_p') = 'number')
       AND (parameters->'top_k' IS NULL OR jsonb_typeof(parameters->'top_k') = 'number')
       AND (parameters->'stream' IS NULL OR jsonb_typeof(parameters->'stream') = 'boolean')
       AND (parameters->'parallel_tool_calls' IS NULL
            OR jsonb_typeof(parameters->'parallel_tool_calls') = 'boolean')
       AND (parameters->'has_system' IS NULL
            OR jsonb_typeof(parameters->'has_system') = 'boolean')
       AND (parameters->'has_structured_output' IS NULL
            OR jsonb_typeof(parameters->'has_structured_output') = 'boolean')
       AND (parameters->'reasoning' IS NULL OR (
           jsonb_typeof(parameters->'reasoning') = 'object'
           AND (parameters->'reasoning') - ARRAY['enabled', 'budget_tokens', 'type', 'effort']
               = '{}'::jsonb
           AND octet_length((parameters->'reasoning')::text) <= 256
       ));
$$;

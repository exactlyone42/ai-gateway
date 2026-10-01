CREATE OR REPLACE FUNCTION valid_gateway_operations(value TEXT[]) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE AS $$
    SELECT cardinality(value) > 0
       AND NOT EXISTS (
           SELECT 1 FROM unnest(value) AS operation
           WHERE operation NOT IN (
               'anthropic_messages', 'anthropic_count_tokens',
               'openai_responses', 'openai_chat_completions',
               'gemini_generate_content'
           )
       );
$$;

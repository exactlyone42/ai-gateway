CREATE FUNCTION provider_target_operation(provider_type TEXT, source_operation TEXT)
RETURNS TEXT
LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE
        WHEN provider_type IN ('openai', 'xai', 'local')
             AND source_operation = 'anthropic_messages'
          THEN 'openai_responses'
        WHEN provider_type = 'anthropic' AND source_operation = 'openai_responses'
          THEN 'anthropic_messages'
        WHEN provider_type = 'gemini' AND source_operation = 'openai_responses'
          THEN 'gemini_generate_content'
        ELSE source_operation
    END;
$$;

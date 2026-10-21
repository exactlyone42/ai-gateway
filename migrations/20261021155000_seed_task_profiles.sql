CREATE OR REPLACE FUNCTION seed_model_task_profiles() RETURNS BIGINT
LANGUAGE sql AS $$
    WITH task(task_kind) AS (
        VALUES
            ('code_generation'), ('code_review'), ('debugging'), ('research'),
            ('quantitative_reasoning'), ('extraction'), ('transformation'),
            ('general_assistance')
    ), scored AS (
        SELECT
            model.tenant_id,
            model.id AS model_id,
            task.task_kind,
            CASE
                WHEN task.task_kind IN ('code_generation', 'code_review', 'debugging')
                     AND (model.public_model_id ILIKE '%coder%'
                          OR model.public_model_id ILIKE '%local-code%') THEN 95
                WHEN task.task_kind IN ('code_generation', 'code_review', 'debugging')
                     AND model.public_model_id ILIKE '%claude%' THEN 85
                WHEN task.task_kind = 'quantitative_reasoning'
                     AND model.supports_reasoning THEN 85
                WHEN task.task_kind = 'research'
                     AND model.supports_reasoning THEN 80
                WHEN task.task_kind IN ('extraction', 'transformation')
                     AND (model.public_model_id ILIKE '%haiku%'
                          OR model.public_model_id ILIKE '%fable%') THEN 90
                WHEN task.task_kind = 'general_assistance'
                     AND (model.public_model_id ILIKE '%sonnet%'
                          OR model.public_model_id ILIKE '%local-general%') THEN 90
                WHEN model.supports_reasoning THEN 75
                ELSE 60
            END::smallint AS suitability_score
        FROM models model CROSS JOIN task
    ), upserted AS (
        INSERT INTO model_task_profiles (
            id, tenant_id, model_id, taxonomy_version, task_kind,
            suitability_score, confidence, source, source_ref, rationale,
            lifecycle_state
        )
        SELECT
            gen_random_uuid(), tenant_id, model_id, 'task-taxonomy-v1', task_kind,
            suitability_score, 0.6000, 'migration', 'model-metadata-v1',
            'Repeatable baseline derived from model identity and declared capabilities.',
            'active'
        FROM scored
        ON CONFLICT (tenant_id, model_id, taxonomy_version, task_kind) DO UPDATE SET
            suitability_score = EXCLUDED.suitability_score,
            confidence = EXCLUDED.confidence,
            source_ref = EXCLUDED.source_ref,
            rationale = EXCLUDED.rationale,
            lifecycle_state = 'active',
            version = model_task_profiles.version + 1,
            updated_at = now()
        WHERE model_task_profiles.source = 'migration'
          AND (model_task_profiles.suitability_score,
               model_task_profiles.confidence,
               model_task_profiles.source_ref,
               model_task_profiles.rationale,
               model_task_profiles.lifecycle_state) IS DISTINCT FROM
              (EXCLUDED.suitability_score,
               EXCLUDED.confidence,
               EXCLUDED.source_ref,
               EXCLUDED.rationale,
               EXCLUDED.lifecycle_state)
        RETURNING 1
    )
    SELECT count(*) FROM upserted;
$$;

SELECT seed_model_task_profiles();

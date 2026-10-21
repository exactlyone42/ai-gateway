CREATE TABLE task_taxonomies (
    taxonomy_version TEXT PRIMARY KEY CHECK (
        taxonomy_version ~ '^[a-z][a-z0-9-]{0,62}$'
    ),
    definition JSONB NOT NULL CHECK (jsonb_typeof(definition) = 'object'),
    fixture_sha256 TEXT NOT NULL CHECK (fixture_sha256 ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TRIGGER task_taxonomies_append_only
    BEFORE UPDATE OR DELETE ON task_taxonomies
    FOR EACH ROW EXECUTE FUNCTION reject_ledger_mutation();
CREATE TRIGGER task_taxonomies_no_truncate
    BEFORE TRUNCATE ON task_taxonomies
    FOR EACH STATEMENT EXECUTE FUNCTION reject_ledger_mutation();

INSERT INTO task_taxonomies (taxonomy_version, definition, fixture_sha256)
VALUES (
    'task-taxonomy-v1',
    '{"tasks":["code_generation","code_review","debugging","research","quantitative_reasoning","extraction","transformation","general_assistance"]}'::jsonb,
    '68b5e75fbfe70da0d40439305a3a2cb0de37ed33c249cbda28cd94e14f55bca0'
);

CREATE TABLE model_task_profiles (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL CHECK (
        tenant_id <> '00000000-0000-0000-0000-000000000000'::uuid
    ),
    model_id UUID NOT NULL,
    taxonomy_version TEXT NOT NULL,
    task_kind TEXT NOT NULL CHECK (task_kind IN (
        'code_generation', 'code_review', 'debugging', 'research',
        'quantitative_reasoning', 'extraction', 'transformation',
        'general_assistance'
    )),
    suitability_score SMALLINT NOT NULL CHECK (suitability_score BETWEEN 0 AND 100),
    confidence NUMERIC(5,4) NOT NULL CHECK (confidence BETWEEN 0 AND 1),
    source TEXT NOT NULL CHECK (source IN (
        'provider', 'benchmark', 'manual', 'migration'
    )),
    source_ref TEXT CHECK (source_ref IS NULL OR length(source_ref) <= 1024),
    rationale TEXT CHECK (rationale IS NULL OR length(rationale) <= 1024),
    lifecycle_state TEXT NOT NULL DEFAULT 'draft' CHECK (
        lifecycle_state IN ('draft', 'active', 'disabled', 'archived')
    ),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id, model_id, taxonomy_version, task_kind),
    FOREIGN KEY (tenant_id, model_id) REFERENCES models (tenant_id, id),
    FOREIGN KEY (taxonomy_version) REFERENCES task_taxonomies (taxonomy_version)
);

CREATE INDEX model_task_profiles_lookup_idx
    ON model_task_profiles (
        tenant_id, taxonomy_version, task_kind, lifecycle_state,
        suitability_score DESC, confidence DESC, model_id
    );

CREATE FUNCTION seed_model_task_profiles() RETURNS BIGINT
LANGUAGE sql AS $$
    WITH inserted AS (
        INSERT INTO model_task_profiles (
            id, tenant_id, model_id, taxonomy_version, task_kind,
            suitability_score, confidence, source, source_ref, rationale
        )
        SELECT
            gen_random_uuid(), model.tenant_id, model.id, 'task-taxonomy-v1',
            'general_assistance', 0, 0, 'migration', 'legacy-model-v1',
            'Unclassified migration placeholder; requires review before activation.'
        FROM models AS model
        WHERE NOT EXISTS (
            SELECT 1
            FROM model_task_profiles AS profile
            WHERE profile.tenant_id = model.tenant_id
              AND profile.model_id = model.id
              AND profile.taxonomy_version = 'task-taxonomy-v1'
        )
        ON CONFLICT DO NOTHING
        RETURNING 1
    )
    SELECT count(*) FROM inserted;
$$;

SELECT seed_model_task_profiles();

CREATE TABLE task_classifier_configs (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL CHECK (
        tenant_id <> '00000000-0000-0000-0000-000000000000'::uuid
    ),
    taxonomy_version TEXT NOT NULL,
    classifier_model_id UUID,
    mode TEXT NOT NULL DEFAULT 'disabled' CHECK (
        mode IN ('disabled', 'shadow', 'enforced')
    ),
    minimum_confidence NUMERIC(5,4) NOT NULL DEFAULT 0.8000 CHECK (
        minimum_confidence BETWEEN 0 AND 1
    ),
    timeout_ms INTEGER NOT NULL DEFAULT 500 CHECK (timeout_ms BETWEEN 1 AND 5000),
    version BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, id),
    UNIQUE (tenant_id),
    FOREIGN KEY (taxonomy_version) REFERENCES task_taxonomies (taxonomy_version),
    FOREIGN KEY (tenant_id, classifier_model_id) REFERENCES models (tenant_id, id),
    CHECK (mode = 'disabled' OR classifier_model_id IS NOT NULL)
);

ALTER TABLE gateway_requests
    ADD COLUMN task_taxonomy_version TEXT,
    ADD COLUMN classified_task TEXT,
    ADD COLUMN classification_confidence NUMERIC(5,4),
    ADD COLUMN capability_version TEXT,
    ADD COLUMN required_capabilities JSONB,
    ADD COLUMN service_objective_version TEXT,
    ADD COLUMN service_objective JSONB,
    ADD COLUMN model_task_profile_id UUID,
    ADD COLUMN model_task_profile_version BIGINT,
    ADD FOREIGN KEY (task_taxonomy_version)
        REFERENCES task_taxonomies (taxonomy_version),
    ADD FOREIGN KEY (tenant_id, model_task_profile_id)
        REFERENCES model_task_profiles (tenant_id, id),
    ADD CONSTRAINT gateway_requests_classified_task_check CHECK (
        classified_task IS NULL OR classified_task IN (
            'code_generation', 'code_review', 'debugging', 'research',
            'quantitative_reasoning', 'extraction', 'transformation',
            'general_assistance'
        )
    ) NOT VALID,
    ADD CONSTRAINT gateway_requests_classification_confidence_check CHECK (
        classification_confidence IS NULL
        OR classification_confidence BETWEEN 0 AND 1
    ) NOT VALID,
    ADD CONSTRAINT gateway_requests_required_capabilities_check CHECK (
        required_capabilities IS NULL
        OR jsonb_typeof(required_capabilities) = 'array'
    ) NOT VALID,
    ADD CONSTRAINT gateway_requests_service_objective_check CHECK (
        service_objective IS NULL OR jsonb_typeof(service_objective) = 'object'
    ) NOT VALID,
    ADD CONSTRAINT gateway_requests_task_classification_pair_check CHECK (
        (classified_task IS NULL AND classification_confidence IS NULL)
        OR (classified_task IS NOT NULL AND task_taxonomy_version IS NOT NULL
            AND classification_confidence IS NOT NULL)
    ) NOT VALID,
    ADD CONSTRAINT gateway_requests_capability_pair_check CHECK (
        (required_capabilities IS NULL) = (capability_version IS NULL)
    ) NOT VALID,
    ADD CONSTRAINT gateway_requests_service_objective_pair_check CHECK (
        (service_objective IS NULL) = (service_objective_version IS NULL)
    ) NOT VALID,
    ADD CONSTRAINT gateway_requests_model_task_profile_pair_check CHECK (
        (model_task_profile_id IS NULL) = (model_task_profile_version IS NULL)
        AND (model_task_profile_version IS NULL OR model_task_profile_version > 0)
    ) NOT VALID;

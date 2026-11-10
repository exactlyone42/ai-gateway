UPDATE quotas SET configuration = '{"mode":"hard","limits":{"requests_per_minute":0}}'::jsonb WHERE id = '00000000-0000-0000-0000-000000002010';
UPDATE quotas SET configuration = '{"mode":"hard","limits":{"requests_per_minute":100}}'::jsonb WHERE id = '00000000-0000-0000-0000-000000002011';

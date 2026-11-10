UPDATE quotas SET configuration = jsonb_set(configuration, '{limits,requests_per_minute}', to_jsonb($1::integer))
WHERE id = '00000000-0000-0000-0000-000000002011'

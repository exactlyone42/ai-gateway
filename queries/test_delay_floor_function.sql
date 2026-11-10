CREATE FUNCTION public.gateway_test_delay_floor() RETURNS event_trigger LANGUAGE plpgsql AS $$
BEGIN
    IF pg_catalog.current_query() LIKE '%capture_erasure_floor%' THEN
        PERFORM pg_catalog.pg_sleep(15);
    END IF;
END
$$;

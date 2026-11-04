CREATE FUNCTION public.witness_test_rollback() RETURNS void
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog AS $$
    UPDATE erasure_witness.position SET sequence = 0, tip = decode(repeat('01', 32), 'hex')
$$;

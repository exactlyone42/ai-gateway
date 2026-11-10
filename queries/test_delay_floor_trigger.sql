CREATE EVENT TRIGGER gateway_test_delay_floor ON ddl_command_start
WHEN TAG IN ('CREATE TABLE') EXECUTE FUNCTION public.gateway_test_delay_floor();

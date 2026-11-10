SELECT current_database() = 'gateway' AND current_user = 'gateway' AND inet_server_addr() IS NULL;

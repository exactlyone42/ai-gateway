SELECT set_config('lock_timeout', '2s', true) AS lock_timeout,
       set_config('statement_timeout', '5s', true) AS statement_timeout;

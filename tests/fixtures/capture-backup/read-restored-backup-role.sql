SET ROLE fixture_backup;
SELECT current_user, value FROM backup_privilege_fixture.probe;

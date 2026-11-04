from datetime import datetime, timedelta
import os
from pathlib import Path
import subprocess
import tempfile
import time
from urllib.parse import urlencode
import uuid

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "tests/fixtures/erasure-witness"


def run(args, **kwargs):
    try:
        return subprocess.run(args, check=True, **kwargs)
    except subprocess.CalledProcessError as error:
        if isinstance(error.stdout, str):
            print(error.stdout, end="", flush=True)
        if isinstance(error.stderr, str):
            print(error.stderr, end="", flush=True)
        raise


def report_one_test(result):
    print(result.stdout, end="", flush=True)
    print(result.stderr, end="", flush=True)
    if "test result: ok. 1 passed; 0 failed; 0 ignored" not in result.stdout:
        raise RuntimeError("隔離測試沒有剛好執行一項")


def execute_test(env):
    report_one_test(run(["cargo", "test", "--lib", "capture_erasure::tests::remote_witness_gates_recovery_and_duplicate_deletions", "--", "--ignored", "--exact"], cwd=ROOT, env=env, timeout=600, capture_output=True, text=True))


def execute_privilege_test(env, safe):
    env = {**env, "TEST_WITNESS_EXPECT_SAFE": "1" if safe else "0"}
    report_one_test(run(["cargo", "test", "--lib", "capture_remote_witness::tests::rejects_later_privilege_changes", "--", "--ignored", "--exact"], cwd=ROOT, env=env, timeout=600, capture_output=True, text=True))


def execute_standby_test(env):
    report_one_test(run(["cargo", "test", "--lib", "capture_remote_witness::tests::rejects_standby_witness", "--", "--ignored", "--exact"], cwd=ROOT, env=env, timeout=600, capture_output=True, text=True))


def main():
    if os.environ.get("DOCKER_HOST"):
        raise RuntimeError("需要本機 Docker")
    context = subprocess.check_output(["docker", "context", "show"], text=True).strip()
    endpoint = subprocess.check_output(["docker", "context", "inspect", context, "--format", "{{.Endpoints.docker.Host}}"], text=True).strip()
    if not endpoint.startswith("unix:///"):
        raise RuntimeError("需要本機 Docker socket")
    env = {**os.environ, "DOCKER_CONTEXT": context}
    suffix = uuid.uuid4().hex[:12]
    name = "capture-witness-tls-" + suffix
    database = "gateway_witness_test_" + suffix
    expiry = "expire=" + (datetime.now() + timedelta(days=1)).date().isoformat()
    created_volumes = []
    started = False
    standby_started = False
    standby = name + "-standby"

    def docker(*args):
        return run(["docker", *args], env=env, capture_output=True, text=True, timeout=30)

    try:
        with tempfile.TemporaryDirectory(prefix="gateway-witness-tls-") as directory:
            directory = Path(directory)
            quiet = {"stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL, "timeout": 30}
            run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=Gateway Test CA", "-keyout", str(directory / "ca.key"), "-out", str(directory / "ca.crt")], **quiet)
            run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=Untrusted Test CA", "-keyout", str(directory / "bad.key"), "-out", str(directory / "bad.crt")], **quiet)
            run(["openssl", "req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost", "-keyout", str(directory / "server.key"), "-out", str(directory / "server.csr")], **quiet)
            run(["openssl", "x509", "-req", "-in", str(directory / "server.csr"), "-CA", str(directory / "ca.crt"), "-CAkey", str(directory / "ca.key"), "-CAcreateserial", "-days", "1", "-extfile", str(FIXTURES / "server-cert.cnf"), "-out", str(directory / "server.crt")], **quiet)
            for volume in (name + "-cert", name + "-data"):
                docker("volume", "create", "--label", expiry, volume)
                created_volumes.append(volume)
            docker("run", "--rm", "--pull=never", "--label", expiry, "--network", "none", "--user", "0", "--mount", f"type=bind,source={directory},target=/input,readonly", "--mount", f"type=volume,source={name}-cert,target=/cert", "postgres:17-alpine", "sh", "-c", "cp /input/server.crt /cert/server.crt && cp /input/server.key /cert/server.key && chown 70:70 /cert/server.crt /cert/server.key && chmod 600 /cert/server.key")
            docker("run", "-d", "--rm", "--pull=never", "--label", expiry, "--name", name, "-p", "127.0.0.1::5432", "-p", "127.0.0.1::5433", "--mount", f"type=volume,source={name}-data,target=/var/lib/postgresql/data", "--mount", f"type=volume,source={name}-cert,target=/cert,readonly", "-e", "POSTGRES_USER=witness_owner", "-e", "POSTGRES_PASSWORD=synthetic-only", "-e", f"POSTGRES_DB={database}", "postgres:17-alpine", "postgres", "-c", "ssl=on", "-c", "ssl_cert_file=/cert/server.crt", "-c", "ssl_key_file=/cert/server.key")
            started = True
            command = ["docker", "exec", "-i", name, "psql", "-X", "-U", "witness_owner", "-d", database, "-Atq", "-v", "ON_ERROR_STOP=1", "-f", "-"]
            deadline = time.monotonic() + 30
            while True:
                result = subprocess.run(command, input=(FIXTURES / "ready.sql").read_bytes(), env=env, capture_output=True, timeout=5)
                if result.returncode == 0:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError("隔離資料庫沒有啟動")
                time.sleep(0.1)
            for source, variables in ((FIXTURES / "tls-roles.sql", {"test_database": database}), (ROOT / "deploy/erasure-witness/schema.sql", {"witness_owner": "gateway_witness_tls_owner"}), (ROOT / "deploy/erasure-witness/grant.sql", {"witness_role": "gateway_witness_login"})):
                args = command[:-2] + sum((["-v", f"{key}={value}"] for key, value in variables.items()), []) + command[-2:]
                result = subprocess.run(args, input=source.read_bytes(), env=env, capture_output=True, timeout=30)
                if result.returncode:
                    raise RuntimeError(f"{source.name}: {result.stderr.decode(errors='replace')}")
            port = docker("port", name, "5432/tcp").stdout.strip().rsplit(":", 1)[1]
            query = urlencode({"sslmode": "verify-full", "sslrootcert": str(directory / "ca.crt")})
            base = f"@127.0.0.1:{port}/{database}?{query}"
            test_env = {**env, "SQLX_OFFLINE": "true", "CARGO_BUILD_JOBS": "2", "TEST_ERASURE_WITNESS_URL": "postgres://gateway_witness_login:synthetic-only" + base, "TEST_ERASURE_WITNESS_ADMIN_URL": "postgres://witness_owner:synthetic-only" + base}
            negative_ca = directory / ("ca.crt" if os.environ.get("TEST_WITNESS_NEGATIVE_CONTROL") == "1" else "bad.crt")
            bad_query = urlencode({"sslmode": "verify-full", "sslrootcert": str(negative_ca)})
            test_env["TEST_ERASURE_WITNESS_BAD_URL"] = f"postgres://gateway_witness_login:synthetic-only@127.0.0.1:{port}/{database}?{bad_query}"
            execute_test(test_env)
            variables = {"install": "00000000-0000-4000-8000-000000000001", "sequence": "0", "tip": "01" * 32}
            args = command[:-2] + sum((["-v", f"{key}={value}"] for key, value in variables.items()), []) + command[-2:]
            run(args, input=(ROOT / "deploy/erasure-witness/bootstrap.sql").read_bytes(), env=env, capture_output=True, timeout=30)
            execute_privilege_test(test_env, True)
            for source, safe in (("grant-login-write.sql", False), ("revoke-login-write.sql", True), ("unsafe-function-create.sql", False), ("unsafe-function-drop.sql", True)):
                run(command, input=(FIXTURES / source).read_bytes(), env=env, capture_output=True, timeout=30)
                execute_privilege_test(test_env, safe)
            standby_volume = name + "-standby-data"
            docker("volume", "create", "--label", expiry, standby_volume)
            created_volumes.append(standby_volume)
            docker("run", "--rm", "--pull=never", "--label", expiry, "--network", "none", "--user", "0", "--mount", f"type=volume,source={standby_volume},target=/standby", "postgres:17-alpine", "chown", "70:70", "/standby")
            docker("run", "--rm", "--pull=never", "--label", expiry, "--network", "container:" + name, "--user", "70", "--mount", f"type=volume,source={standby_volume},target=/standby", "-e", "PGPASSWORD=synthetic-only", "postgres:17-alpine", "pg_basebackup", "-h", "127.0.0.1", "-p", "5432", "-U", "witness_owner", "-D", "/standby", "-R", "-X", "stream")
            docker("run", "-d", "--rm", "--pull=never", "--label", expiry, "--name", standby, "--network", "container:" + name, "--mount", f"type=volume,source={standby_volume},target=/var/lib/postgresql/data", "--mount", f"type=volume,source={name}-cert,target=/cert,readonly", "postgres:17-alpine", "postgres", "-c", "port=5433", "-c", "ssl=on", "-c", "ssl_cert_file=/cert/server.crt", "-c", "ssl_key_file=/cert/server.key")
            standby_started = True
            standby_command = ["docker", "exec", "-i", standby, "psql", "-X", "-h", "127.0.0.1", "-p", "5433", "-U", "witness_owner", "-d", database, "-Atq", "-v", "ON_ERROR_STOP=1", "-f", "-"]
            deadline = time.monotonic() + 30
            while True:
                result = subprocess.run(standby_command, input=(FIXTURES / "is-standby.sql").read_bytes(), env=env, capture_output=True, timeout=5)
                if result.returncode == 0 and result.stdout.strip() == b"t":
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError("隔離備援資料庫沒有啟動")
                time.sleep(0.1)
            old_position = run(standby_command, input=(ROOT / "queries/erasure_witness_get.sql").read_bytes(), env=env, capture_output=True, timeout=30).stdout
            run(standby_command, input=(FIXTURES / "pause-replay.sql").read_bytes(), env=env, capture_output=True, timeout=30)
            deadline = time.monotonic() + 10
            while True:
                result = run(standby_command, input=(FIXTURES / "replay-paused.sql").read_bytes(), env=env, capture_output=True, timeout=30)
                if result.stdout.strip() == b"t":
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError("備援資料庫未暫停重播")
                time.sleep(0.1)
            advanced = run(command, input=(FIXTURES / "standby-advance.sql").read_bytes(), env=env, capture_output=True, timeout=30)
            if advanced.stdout.strip() != b"t":
                raise RuntimeError("主資料庫沒有推進見證")
            new_position = run(command, input=(ROOT / "queries/erasure_witness_get.sql").read_bytes(), env=env, capture_output=True, timeout=30).stdout
            standby_position = run(standby_command, input=(ROOT / "queries/erasure_witness_get.sql").read_bytes(), env=env, capture_output=True, timeout=30).stdout
            if new_position == old_position or standby_position != old_position:
                raise RuntimeError("沒有建立落後的備援位置")
            standby_port = docker("port", name, "5433/tcp").stdout.strip().rsplit(":", 1)[1]
            test_env["TEST_ERASURE_WITNESS_STANDBY_URL"] = f"postgres://gateway_witness_login:synthetic-only@127.0.0.1:{standby_port}/{database}?{query}"
            execute_standby_test(test_env)
            execute_privilege_test(test_env, True)
            run(standby_command, input=(FIXTURES / "resume-replay.sql").read_bytes(), env=env, capture_output=True, timeout=30)
            deadline = time.monotonic() + 10
            while True:
                current = run(standby_command, input=(ROOT / "queries/erasure_witness_get.sql").read_bytes(), env=env, capture_output=True, timeout=30).stdout
                if current == new_position:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError("備援資料庫未追上主資料庫")
                time.sleep(0.1)
            promoted = run(standby_command, input=(FIXTURES / "promote.sql").read_bytes(), env=env, capture_output=True, timeout=40)
            if promoted.stdout.strip() != b"t":
                raise RuntimeError("隔離備援資料庫未升格")
            execute_privilege_test({**test_env, "TEST_ERASURE_WITNESS_URL": test_env["TEST_ERASURE_WITNESS_STANDBY_URL"]}, True)
    finally:
        failures = []
        if standby_started:
            try:
                docker("rm", "-f", "-v", standby)
            except (OSError, subprocess.SubprocessError):
                failures.append(standby)
        if started:
            try:
                docker("rm", "-f", "-v", name)
            except (OSError, subprocess.SubprocessError):
                failures.append(name)
        for volume in reversed(created_volumes):
            try:
                docker("volume", "rm", volume)
            except (OSError, subprocess.SubprocessError):
                failures.append(volume)
        if failures:
            raise RuntimeError("隔離資源清理失敗：" + ", ".join(failures))


if __name__ == "__main__":
    main()

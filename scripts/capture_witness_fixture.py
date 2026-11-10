from pathlib import Path
import time


def start(docker, run, root, workspace, network, expiry, label, created, suffix):
    name = "capture-e2e-witness-" + suffix
    volume = "capture-e2e-witness-cert-" + suffix
    config = workspace / "witness-cert.cnf"
    config.write_text(f"subjectAltName = DNS:{name}\nextendedKeyUsage = serverAuth\nbasicConstraints = CA:FALSE\n")
    ca = workspace / "witness-ca.crt"
    key = workspace / "witness-server.key"
    cert = workspace / "witness-server.crt"
    ca_key = workspace / "witness-ca.key"
    request = workspace / "witness-server.csr"
    run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=Gateway Test CA", "-keyout", str(ca_key), "-out", str(ca))
    run("openssl", "req", "-newkey", "rsa:2048", "-nodes", "-subj", f"/CN={name}", "-keyout", str(key), "-out", str(request))
    run("openssl", "x509", "-req", "-in", str(request), "-CA", str(ca), "-CAkey", str(ca_key), "-CAcreateserial", "-days", "1", "-extfile", str(config), "-out", str(cert))
    ca.chmod(0o644)
    created.append(("volume", volume))
    docker("volume", "create", "--label", expiry, "--label", label, volume)
    docker("run", "--rm", "--pull=never", "--label", expiry, "--label", label, "--network", "none", "--user", "0", "--mount", f"type=bind,source={workspace},target=/input,readonly", "--mount", f"type=volume,source={volume},target=/cert", "postgres:17-alpine", "sh", "-c", "cp /input/witness-server.crt /cert/server.crt && cp /input/witness-server.key /cert/server.key && chown 70:70 /cert/server.crt /cert/server.key && chmod 600 /cert/server.key")
    created.append(("container", name))
    docker("run", "-d", "--rm", "--pull=never", "--label", expiry, "--label", label, "--name", name, "--network", network, "--mount", f"type=volume,source={volume},target=/cert,readonly", "-e", "POSTGRES_USER=witness_owner", "-e", "POSTGRES_PASSWORD=synthetic-only", "-e", "POSTGRES_DB=witness", "postgres:17-alpine", "postgres", "-c", "ssl=on", "-c", "ssl_cert_file=/cert/server.crt", "-c", "ssl_key_file=/cert/server.key")
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            docker("exec", name, "pg_isready", "-h", "127.0.0.1", "-U", "witness_owner")
            break
        except Exception:
            time.sleep(0.2)
    else:
        raise RuntimeError("見證測試資料庫無法啟動")
    execute(docker, name, root / "tests/fixtures/erasure-witness/tls-roles.sql", test_database="witness")
    execute(docker, name, root / "deploy/erasure-witness/schema.sql", witness_owner="gateway_witness_tls_owner")
    execute(docker, name, root / "deploy/erasure-witness/grant.sql", witness_role="gateway_witness_login")
    url = f"postgres://gateway_witness_login:synthetic-only@{name}/witness?sslmode=verify-full&sslrootcert=/tmp/witness-ca.crt"
    return name, ca, url


def execute(docker, name, path, **variables):
    args = sum((["-v", f"{key}={value}"] for key, value in variables.items()), [])
    return docker("exec", "-i", name, "psql", "-X", "-U", "witness_owner", "-d", "witness", "-Atq", "-v", "ON_ERROR_STOP=1", *args, "-f", "-", input=Path(path).read_text())


def bootstrap(docker, name, root, position):
    execute(docker, name, root / "deploy/erasure-witness/bootstrap.sql", install=position["install"], sequence=position["sequence"], tip=bytes(position["tip"]).hex())


def read(docker, name, root):
    install, sequence, tip = execute(docker, name, root / "queries/erasure_witness_get.sql").split("|")
    return {"install": install, "sequence": int(sequence), "tip": tip.removeprefix("\\x")}

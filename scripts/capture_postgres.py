from pathlib import Path
import re
import subprocess


def configured_port(info):
    ports = [entry.split("=", 1)[1] for entry in info["Config"]["Env"] if entry.startswith("PGPORT=")]
    if len(ports) > 1 or (ports and (not ports[0].isdigit() or not 1 <= int(ports[0]) <= 65535)):
        raise ValueError("invalid PostgreSQL port")
    return int(ports[0]) if ports else 5432


def local_exec(identifier, port, program):
    names = ("PGHOST", "PGHOSTADDR", "PGPORT", "PGSERVICE", "PGSERVICEFILE", "PGDATABASE", "PGUSER", "PGOPTIONS")
    unset = sum((["-u", name] for name in names), [])
    command = ["docker", "exec", "-i", identifier, "env", *unset, program, "-h", "/var/run/postgresql", "-p", str(port)]
    return command + (["-X"] if program == "psql" else [])



def valid_database_identity(system_id, oid):
    if not isinstance(system_id, str) or not re.fullmatch(r"-?[0-9]{1,20}", system_id) or not isinstance(oid, str) or not re.fullmatch(r"[0-9]{1,10}", oid):
        return False
    return -(2**63) <= int(system_id) < 2**63 and 0 <= int(oid) <= 4294967295


def database_identity(identifier, port, user, database):
    query = Path(__file__).resolve().parent.parent / "queries" / "capture_backup_database_identity.sql"
    with query.open("rb") as source:
        result = subprocess.run(local_exec(identifier, port, "psql") + ["-U", user, "-d", database, "-Atq", "-v", "ON_ERROR_STOP=1"], stdin=source, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5, check=False)
    match = re.fullmatch(rb"(-?[0-9]{1,20})\|([0-9]{1,10})\s*", result.stdout)
    if result.returncode or not match or not valid_database_identity(match[1].decode("ascii"), match[2].decode("ascii")):
        raise ValueError("無法確認資料庫身分")
    return match[1].decode("ascii"), match[2].decode("ascii")

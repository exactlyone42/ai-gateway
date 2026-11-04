import hashlib
import json
import os
import subprocess


class StorageIdentityError(Exception):
    pass


def command(args):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10, check=False)
    if result.returncode:
        raise StorageIdentityError("無法確認儲存來源")
    return result.stdout


def require_local_docker():
    host = os.environ.get("DOCKER_HOST", "")
    if host and not host.startswith("unix:///"):
        raise StorageIdentityError("僅支援本機 Docker")
    if os.environ.get("DOCKER_CONTEXT") or not host:
        context = command(["docker", "context", "show"]).decode("utf-8").strip()
        details = json.loads(command(["docker", "context", "inspect", context]))
        host = details[0]["Endpoints"]["docker"]["Host"]
    if not isinstance(host, str) or not host.startswith("unix:///"):
        raise StorageIdentityError("僅支援本機 Docker")


def storage_identity(mounts, expected=5):
    require_local_docker()
    if type(expected) is not int or expected not in (1, 5) or len(mounts) != expected:
        raise StorageIdentityError("保存掛載不完整")
    if any(mount.get("Type") != "volume" for mount in mounts):
        raise StorageIdentityError("目錄掛載尚無法安全核對")
    names = [mount.get("Name") for mount in mounts if mount.get("Type") == "volume"]
    if any(not isinstance(name, str) or not name for name in names) or len(names) != len(set(names)):
        raise StorageIdentityError("磁碟卷識別不正確")
    volumes = json.loads(command(["docker", "volume", "inspect", *names])) if names else []
    if not isinstance(volumes, list) or len(volumes) != len(names):
        raise StorageIdentityError("無法確認磁碟卷")
    indexed = {volume.get("Name"): volume for volume in volumes if isinstance(volume, dict)}
    if set(indexed) != set(names):
        raise StorageIdentityError("磁碟卷識別不一致")
    identities = []
    for mount in mounts:
        destination = mount.get("Destination")
        if not isinstance(destination, str) or not destination.startswith("/"):
            raise StorageIdentityError("保存掛載位置不正確")
        if mount.get("Type") == "volume":
            volume = indexed[mount["Name"]]
            fields = {key: volume.get(key) for key in ("Name", "Driver", "Scope", "CreatedAt")}
            if any(not isinstance(value, str) or not value for value in fields.values()):
                raise StorageIdentityError("磁碟卷資訊不完整")
            identities.append({"destination": destination, "type": "volume", "identity": fields})
    encoded = json.dumps(identities, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode("ascii")
    return hashlib.sha256(b"capture-storage-v1\0" + encoded).hexdigest()

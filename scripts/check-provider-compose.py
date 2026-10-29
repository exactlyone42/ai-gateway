import json
import os
import subprocess

environment = os.environ.copy()
environment["EXTRA_PROVIDER_PROFILES_JSON"] = json.dumps([{"provider_key": "compose-check"}])
environment["LOCAL_PROVIDER_IDLE_TIMEOUT_MS"] = "234567"
environment["CONTENT_CAPTURE_ENABLED"] = "false"
result = subprocess.check_output(
    ["docker", "compose", "-f", "deploy/docker-compose.yml", "config", "--format", "json"],
    env=environment,
)
config = json.loads(result)
forwarded = config["services"]["gateway"]["environment"]["EXTRA_PROVIDER_PROFILES_JSON"]
if json.loads(forwarded)[0]["provider_key"] != "compose-check":
    raise SystemExit("provider settings were not forwarded to the gateway")
if config["services"]["gateway"]["environment"]["LOCAL_PROVIDER_IDLE_TIMEOUT_MS"] != "234567":
    raise SystemExit("local provider timeout was not forwarded to the gateway")
gateway = config["services"]["gateway"]
if gateway["environment"]["CONTENT_CAPTURE_ENABLED"] != "false":
    raise SystemExit("content capture setting was not forwarded to the gateway")
paths = [gateway["environment"][name] for name in ("CONTENT_CAPTURE_SPOOL_DIR", "CONTENT_CAPTURE_KEY_DIR", "CONTENT_CAPTURE_ANCHOR_DIR")]
mounts = {mount["target"]: mount for mount in gateway["volumes"]}
if len(set(paths)) != 3 or any(path not in mounts or mounts[path]["type"] != "volume" for path in paths):
    raise SystemExit("content capture directories need three separate volumes")
if len({mounts[path]["source"] for path in paths}) != 3:
    raise SystemExit("content capture volumes must be distinct")

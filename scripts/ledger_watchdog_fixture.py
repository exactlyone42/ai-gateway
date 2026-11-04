import json
import os
from pathlib import Path
import re
import subprocess
import sys


def docker(*args):
    return subprocess.run(["docker", *args], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=30, check=True).stdout


def main():
    if len(sys.argv) not in (3, 4):
        return 1
    container, nonce = sys.argv[1:3]
    destination = sys.argv[3] if len(sys.argv) == 4 else None
    destination_fd = None
    if destination is not None and destination.startswith("fd:"):
        if not destination[3:].isdigit():
            return 1
        destination_fd = int(destination[3:])
    elif destination is not None and not Path(destination).is_absolute():
        return 1
    if not re.fullmatch(r"[0-9a-f]{64}", container) or not re.fullmatch(r"[0-9a-f]{32}", nonce):
        return 1
    info = json.loads(docker("inspect", container))[0]
    if info["Id"] != container or info["Config"]["Labels"].get("gateway.ledger_watchdog_test") != nonce or not info["State"]["Running"]:
        return 1
    networks = info["NetworkSettings"]["Networks"]
    if info["HostConfig"]["NetworkMode"] != "none":
        if len(networks) != 1:
            return 1
        network = json.loads(docker("network", "inspect", next(iter(networks))))[0]
        if not network.get("Internal") or (network.get("Labels") or {}).get("gateway.ledger_watchdog_test") != nonce:
            return 1
    stopping = False
    try:
        stopping = True
        stop = subprocess.Popen(["docker", "stop", "--time", "5", container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        print("STOPPING", stop.pid, flush=True)
        if stop.wait(timeout=30) != 0:
            return 1
        info = json.loads(docker("inspect", container))[0]
        if info["State"]["Status"] != "exited" or info["State"]["ExitCode"] != 0:
            return 1
        print("STOPPED", flush=True)
        action = sys.stdin.buffer.readline()
        if action == b"commit\n" and destination is not None:
            directory = os.dup(destination_fd) if destination_fd is not None else os.open(destination, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
            try:
                descriptor = os.open(".complete", os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory)
                try:
                    os.write(descriptor, b"ok\n")
                    os.fsync(descriptor)
                finally:
                    os.close(descriptor)
                os.fsync(directory)
            finally:
                os.close(directory)
    finally:
        if stopping:
            info = json.loads(docker("inspect", container))[0]
            if info["Id"] != container or info["Config"]["Labels"].get("gateway.ledger_watchdog_test") != nonce:
                return 1
            if not info["State"]["Running"]:
                docker("start", container)
            info = json.loads(docker("inspect", container))[0]
            if not info["State"]["Running"]:
                return 1
            print("RESTARTED", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())

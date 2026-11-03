#!/usr/bin/env python3
"""Preflight the pinned database with fresh CSI-like volumes; never use live PVCs."""
import os
from pathlib import Path
import shlex
import subprocess
import time
import uuid

import yaml

ROOT = Path(__file__).resolve().parents[2]
values = yaml.safe_load((ROOT / "deploy/supabase/values.yaml").read_text())
init = values["deployment"]["db"]["extraInitContainers"][0]
image = init["image"]
assert image.endswith("@sha256:7374d196da7e301512cc5329e18040e0f7bb856287b323777cdadecccbc6c7e4")
runtime = shlex.split(os.environ.get("SUPABASE_TEST_CONTAINER_COMMAND", "docker"))
prefix = "civpulse-db-preflight-" + uuid.uuid4().hex[:12]
config, data, database = prefix + "-config", prefix + "-data", prefix + "-db"
capabilities = values["deployment"]["db"]["securityContext"]["capabilities"]
security = ["--cap-drop", "ALL", "--security-opt", "no-new-privileges"]
for capability in capabilities["add"]:
    security += ["--cap-add", capability]
assert capabilities["drop"] == ["ALL"]


def run(*args, check=True, timeout=120):
    result = subprocess.run([*runtime, *args], capture_output=True, timeout=timeout)
    if check and result.returncode:
        # Do not dump container output: startup helpers can handle secret key bytes.
        raise RuntimeError("Pinned Supabase preflight command failed (output suppressed)")
    return result


def shell(script, mounts):
    run("run", "--rm", "--network", "none", *security, *mounts,
        "--entrypoint", "/bin/sh", image, "-ec", script)


def ready():
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        if run("exec", database, "pg_isready", "-h", "127.0.0.1", "-U",
               "supabase_admin", "-d", "postgres", check=False).returncode == 0:
            return
        if run("inspect", "--format", "{{.State.Running}}", database).stdout.strip() != b"true":
            raise RuntimeError("Pinned database exited before TCP readiness (logs suppressed)")
        time.sleep(1)
    raise RuntimeError("Pinned database did not become TCP-ready before the deadline")


try:
    run("volume", "create", config)
    run("volume", "create", data)
    init_mounts = ["-v", config + ":/mnt/pgsodium:nocopy", "-v", data + ":/mnt/data:nocopy"]
    shell("""
chown root:root /mnt/pgsodium /mnt/data
chmod 0755 /mnt/pgsodium /mnt/data
mkdir -p /mnt/pgsodium/lost+found
chmod 0700 /mnt/pgsodium/lost+found
""" + init["args"][0] + """
test "$(stat -c '%u:%g:%a' /mnt/pgsodium)" = "$(id -u postgres):$(id -g postgres):755"
test "$(stat -c '%u:%g:%a' /mnt/pgsodium/lost+found)" = '0:0:700'
""", init_mounts)
    key_mount = ["-v", config + ":/etc/postgresql-custom:nocopy"]
    shell("""
gosu postgres /usr/lib/postgresql/bin/pgsodium_getkey.sh > /dev/null
test -s /etc/postgresql-custom/pgsodium_root.key
test "$(wc -c < /etc/postgresql-custom/pgsodium_root.key)" -eq 64
cp -a /etc/postgresql-custom/pgsodium_root.key /etc/postgresql-custom/.preflight-key-copy
""", key_mount)
    # Reapply the exact init command; neither ownership repair nor copying may rotate keys.
    shell(init["args"][0], init_mounts)
    run("run", "--detach", "--name", database, "--network", "none", *security,
        "--memory", "768m", "--cpus", "1", *key_mount,
        "-v", data + ":/var/lib/postgresql/data:nocopy",
        "-e", "POSTGRES_PASSWORD=local-preflight-password",
        "-e", "PGPASSWORD=local-preflight-password",
        "-e", "JWT_SECRET=local-preflight-fixture-secret-at-least-32-characters", image)
    ready()
    query = "create extension pgsodium; select count(*) from pg_roles where rolname in ('postgres','supabase_admin','anon','authenticated','service_role'); select count(*) from pg_extension where extname='pgsodium';"
    result = run("exec", database, "psql", "-h", "127.0.0.1", "-U", "supabase_admin",
                 "-d", "postgres", "-v", "ON_ERROR_STOP=1", "-qAtc", query)
    assert result.stdout.splitlines() == [b"5", b"1"], "Pinned Supabase baseline role/extension counts: " + repr(result.stdout.splitlines())
    run("restart", database)
    ready()
    run("exec", "--user", "postgres", database, "/bin/sh", "-ec",
        "test \"$(stat -c '%u:%g:%a' /etc/postgresql-custom/pgsodium_root.key)\" = "
        "\"$(stat -c '%u:%g:%a' /etc/postgresql-custom/.preflight-key-copy)\"; "
        "cmp /etc/postgresql-custom/pgsodium_root.key /etc/postgresql-custom/.preflight-key-copy; "
        "/usr/lib/postgresql/bin/pgsodium_getkey.sh > /dev/null; "
        "cmp /etc/postgresql-custom/pgsodium_root.key /etc/postgresql-custom/.preflight-key-copy")
    print("Pinned Supabase startup/restart, CSI-like permissions and persistent key preservation passed")
finally:
    run("rm", "--force", database, check=False)
    # Only UUID-named disposable test volumes created above are removed.
    run("volume", "rm", "--force", config, data, check=False)

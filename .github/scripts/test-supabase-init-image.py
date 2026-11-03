#!/usr/bin/env python3
"""Exercise the actual init command with the digest-pinned image's BusyBox tools."""
import os
from pathlib import Path
import shlex
import subprocess

import yaml

ROOT = Path(__file__).resolve().parents[2]
values = yaml.safe_load((ROOT / "deploy/supabase/values.yaml").read_text())
init = values["deployment"]["db"]["extraInitContainers"][0]
assert init["name"] == "complete-pgsodium-config"
image = init["image"]
assert image.endswith("@sha256:7374d196da7e301512cc5329e18040e0f7bb856287b323777cdadecccbc6c7e4")
command = init["args"][0].replace("/mnt/pgsodium", "/tmp/test-pgsodium")
setup = """
mkdir -p /tmp/test-pgsodium/lost+found
printf '%s' 'existing private key' > /tmp/test-pgsodium/existing-private-key
"""
verify_initial = """
for config in auto_explain.conf custom_overrides.conf generated_optimizations.conf pg_cron.conf pg_net.conf; do
  test -f "/tmp/test-pgsodium/conf.d/$config"
done
test -d /tmp/test-pgsodium/lost+found
cmp /etc/postgresql-custom/wal-g.conf /tmp/test-pgsodium/wal-g.conf
printf '%s' 'existing private key' > /tmp/test-pgsodium/conf.d/pgsodium.key
printf '%s' 'existing custom configuration' > /tmp/test-pgsodium/wal-g.conf
rm /tmp/test-pgsodium/conf.d/pg_cron.conf
ln -s missing-custom-target /tmp/test-pgsodium/dangling-custom-link
printf '%s' 'existing custom linked configuration' > /tmp/custom-optimizations.conf
rm /tmp/test-pgsodium/conf.d/generated_optimizations.conf
ln -s /tmp/custom-optimizations.conf /tmp/test-pgsodium/conf.d/generated_optimizations.conf
"""
verify_rerun = """
test "$(cat /tmp/test-pgsodium/existing-private-key)" = 'existing private key'
test "$(cat /tmp/test-pgsodium/conf.d/pgsodium.key)" = 'existing private key'
test "$(cat /tmp/test-pgsodium/wal-g.conf)" = 'existing custom configuration'
cmp /etc/postgresql-custom/conf.d/pg_cron.conf /tmp/test-pgsodium/conf.d/pg_cron.conf
test -L /tmp/test-pgsodium/dangling-custom-link
test -L /tmp/test-pgsodium/conf.d/generated_optimizations.conf
test "$(cat /tmp/test-pgsodium/conf.d/generated_optimizations.conf)" = 'existing custom linked configuration'
test -d /tmp/test-pgsodium/lost+found
"""
runtime = shlex.split(os.environ.get("SUPABASE_TEST_CONTAINER_COMMAND", "docker"))
subprocess.run(
    [*runtime, "run", "--rm", "--network", "none", "--entrypoint", "/bin/sh", image,
     "-ec", "\n".join([setup, command, verify_initial, command, verify_rerun])],
    check=True,
    timeout=300,
)
print("Pinned Supabase image: fresh config, nested repair, and persisted key/config preservation passed")

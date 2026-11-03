#!/usr/bin/env python3
"""Check that bootstrap cannot accidentally select prod or execute a dry run."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import yaml
from unittest.mock import patch

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "deploy/supabase/bootstrap.py"
migration_lock = json.loads((ROOT / "deploy/supabase/migrations.lock.json").read_text())
assert migration_lock["commit"] == "3d135baa537d315244e6abef0261d4f1de39e1dd"
assert len(migration_lock["files"]) == 136
assert max(migration_lock["files"]) == "20261125003500_sos_results_poll_cadence.sql"
spec = importlib.util.spec_from_file_location("supabase_bootstrap", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
for environment in ("dev", "test"):
    with patch.object(sys, "argv", [str(SCRIPT), environment]), patch.object(module, "run", side_effect=AssertionError("dry run executed a command")):
        module.main()
for arguments in (["prod", "--apply"], ["dev", "--context", "thor", "--apply"]):
    result = subprocess.run([sys.executable, str(SCRIPT), *arguments], capture_output=True)
    assert result.returncode == 2
try:
    with patch.object(module.urllib.request, "urlopen") as opened:
        opened.return_value.__enter__.return_value.read.return_value = b"corrupted migration"
        module.download("https://example.invalid/file.sql", "0"*64)
except RuntimeError:
    pass
else:
    raise AssertionError("corrupted pinned migration accepted")
print("Bootstrap production/context/download guards passed")

# Execute the actual values command against filesystem states matching a fresh
# DO block volume and a previously initialized volume; preserve existing keys.
values = yaml.safe_load((ROOT / "deploy/supabase/values.yaml").read_text())
init = values["deployment"]["db"]["extraInitContainers"][0]
assert init["name"] == "complete-pgsodium-config"
assert init["image"].endswith("@sha256:7374d196da7e301512cc5329e18040e0f7bb856287b323777cdadecccbc6c7e4")
with tempfile.TemporaryDirectory() as directory:
    source, target = Path(directory)/"source", Path(directory)/"target"
    source.mkdir(); target.mkdir(); (target/"lost+found").mkdir()
    (source/"conf.d").mkdir()
    for name in ("wal-g.conf", "read-replica.conf", "supautils.conf", "conf.d/pgsodium.key", "conf.d/pg_cron.conf", ".image-default"):
        (source/name).write_text("image default")
    command = init["args"][0].replace("/etc/postgresql-custom", str(source)).replace("/mnt/pgsodium", str(target))
    # Ownership is checked by the pinned-image test; this host-only copy test
    # runs without root and without assuming the image's postgres UID exists.
    ownership = f"chown postgres:postgres {target}"
    assert ownership in command and "chown -R" not in command
    command = command.replace(ownership, ": # ownership tested in the actual image")
    subprocess.run([*init["command"], command], check=True)
    assert (target/"wal-g.conf").read_text() == "image default"
    (target/"conf.d/pgsodium.key").write_text("existing private key")
    (target/"wal-g.conf").write_text("existing custom configuration")
    (target/"read-replica.conf").unlink()
    (target/"conf.d/pg_cron.conf").unlink()
    (target/"dangling-custom-link").symlink_to("missing-custom-target")
    subprocess.run([*init["command"], command], check=True)
    assert (target/"conf.d/pgsodium.key").read_text() == "existing private key"
    assert (target/"wal-g.conf").read_text() == "existing custom configuration"
    assert (target/"read-replica.conf").read_text() == "image default"
    assert (target/"conf.d/pg_cron.conf").read_text() == "image default"
    assert (target/".image-default").read_text() == "image default"
    assert (target/"dangling-custom-link").is_symlink()
    assert (target/"lost+found").is_dir()
print("Fresh block-volume and existing key/config preservation checks passed")

#!/usr/bin/env python3
"""Check that bootstrap cannot accidentally select prod or execute a dry run."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
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

#!/usr/bin/env python3
"""Render the hash-verified upstream chart and verify only approved rollout changes."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import urllib.request

import yaml

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
patcher = ROOT / "deploy/supabase/patch-chart.py"
lock = json.loads((ROOT / "deploy/supabase/chart.lock.json").read_text())
archive = os.environ.get("SUPABASE_TEST_CHART_ARCHIVE")
if archive:
    chart = Path(archive).read_bytes()
else:
    with urllib.request.urlopen(lock["url"], timeout=120) as response:
        chart = response.read(20 * 1024 * 1024 + 1)
assert hashlib.sha256(chart).hexdigest() == lock["sha256"]
spec = importlib.util.spec_from_file_location("supabase_rollout_patch", patcher)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
patched = module.patch_chart(chart, lock)
assert subprocess.check_output([sys.executable, str(patcher)], input=chart) == patched
failed = subprocess.run([sys.executable, str(patcher)], input=b"tampered", capture_output=True)
assert failed.returncode != 0 and failed.stdout == b""
assert patched == module.patch_chart(chart, lock), "Derived chart is not deterministic"
import io
import tarfile
with tarfile.open(fileobj=io.BytesIO(chart), mode="r:gz") as original_archive, tarfile.open(fileobj=io.BytesIO(patched), mode="r:gz") as patched_archive:
    original_members = {member.name: original_archive.extractfile(member).read() for member in original_archive if member.isfile()}
    patched_members = {member.name: patched_archive.extractfile(member).read() for member in patched_archive if member.isfile()}
assert original_members.keys() == patched_members.keys()
for name, content in original_members.items():
    if name in module.TEMPLATES:
        assert patched_members[name] == module.patch_template(name, content)
        assert module.patch_template(name, patched_members[name]) == patched_members[name]
        try:
            module.patch_template(name, content+b"\n")
        except ValueError:
            pass
        else:
            raise AssertionError("Changed template was accepted")
    else:
        assert patched_members[name] == content, "Unrelated chart member changed"
for data, expected_lock in ((chart+b"x",lock),(chart,{**lock,"version":"0.9.0"})):
    try:
        module.patch_chart(data,expected_lock)
    except ValueError:
        pass
    else:
        raise AssertionError("Unexpected upstream chart accepted")
with tempfile.TemporaryDirectory() as directory:
    pinned = Path(directory)/"supabase.tgz"
    pinned.write_bytes(chart)
    derived = Path(directory)/"patched-supabase.tgz"
    derived.write_bytes(patched)
    for environment in ("dev", "test"):
        command = ["helm", "template", "supabase", str(pinned), "--namespace", "civpulse-"+environment,
                   "-f", str(ROOT/"deploy/supabase/values.yaml")]
        original = list(yaml.safe_load_all(subprocess.check_output(command)))
        rendered = list(yaml.safe_load_all(subprocess.check_output([str(derived) if entry == str(pinned) else entry for entry in command])))
        expected = copy.deepcopy(original)
        for document in expected:
            if document and document.get("kind") == "Deployment" and document["metadata"]["name"] in {"supabase-supabase-storage", "supabase-supabase-rest"}:
                assert document["spec"]["replicas"] == 1
                document["spec"]["strategy"] = {"type":"Recreate", "rollingUpdate":None}
        assert rendered == expected, "Reviewed patch changed unrelated resources"
        for document in rendered:
            if document and document.get("kind") == "StatefulSet":
                assert document["spec"].get("podManagementPolicy", "OrderedReady") == "OrderedReady"
print("Pinned Supabase chart: scoped Recreate, cleared surge, unchanged member bytes, deterministic/idempotent insertion and fail-closed guards passed")

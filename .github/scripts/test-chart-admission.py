#!/usr/bin/env python3
"""Pass actual YAML-rendered numeric caps through the Rust Admission parser."""
import os
from pathlib import Path
import subprocess
import tempfile

import yaml

ROOT = Path(__file__).resolve().parents[2]
checker = ROOT / "target/debug/examples/check_admission"
for download, scope in ((536870912, 67108864), (134217728, 16777216)):
    with tempfile.TemporaryDirectory() as directory:
        override = Path(directory)/"caps.yaml"
        # YAML values (not --set integers) reproduce Helm's float64 formatting path.
        override.write_text(yaml.safe_dump({"collection":{"maxDownloadBytes":download,"maxScopeBytes":scope}}))
        rendered = subprocess.check_output(["helm", "template", "results", str(ROOT/"deploy/helm/civpulse-results-collector"),
            "--namespace", "civpulse-dev", "-f", str(ROOT/"deploy/environments/dev.yaml"), "-f", str(override),
            "--set-string", "image.digest=sha256:"+"a"*64])
        config = next(document["data"] for document in yaml.safe_load_all(rendered) if document and document["kind"] == "ConfigMap")
        assert config["SOS_COLLECTOR_ENABLED"] == "false"
        assert config["SOS_MAX_DOWNLOAD_BYTES"] == str(download)
        assert config["SOS_MAX_SCOPE_BYTES"] == str(scope)
        caps = {key:config[key] for key in ("SOS_MAX_DOWNLOAD_BYTES", "SOS_MAX_SCOPE_BYTES")}
        result = subprocess.run([str(checker)], env={**os.environ,**caps}, capture_output=True, check=True)
        assert result.stdout.split() == [str(download).encode(),str(scope).encode()]
print("Actual chart default/lower integer caps passed strict Rust Admission parsing")

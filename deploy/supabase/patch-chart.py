#!/usr/bin/env python3
"""Apply two reviewed rollout insertions to the exact pinned upstream archive."""
import copy
import gzip
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile

UPSTREAM_SHA = "5aa07424e1137733642e3d765f0d64cd770cf929933a589459f4d2ad005e1416"
TEMPLATES = {
    "supabase/templates/storage/deployment.yaml": (
        "storage", "ab1f5440cf93994b3c77a5730d2b222e54037af92583cb3ed465e069a6dc0110",
        "1a2c01a6713483b186ba92245cdc2d7010d46b7b362516a24e3bc0e540b25d8b"),
    "supabase/templates/rest/deployment.yaml": (
        "rest", "0bec6c6620cdf0a457a797465585b0878cb31edaed0917e4a0825a680043f4cc",
        "fc5c0d2f098d9d215ccad5de29711860387a4b98e79f3a8fa2c0ee1c2064b0fc"),
}


def patch_template(name, content):
    component, original, patched = TEMPLATES[name]
    digest = hashlib.sha256(content).hexdigest()
    if digest == patched:
        return content  # Reapplying the reviewed insertion cannot duplicate it.
    if digest != original:
        raise ValueError("Unreviewed Supabase deployment template")
    anchor = ("spec:\n  {{- if not .Values.autoscaling."+component+".enabled }}").encode()
    if content.count(anchor) != 1:
        raise ValueError("Supabase rollout insertion anchor changed")
    result = content.replace(anchor, b"spec:\n  strategy:\n    type: Recreate\n    rollingUpdate: null\n"+anchor[len(b"spec:\n"):])
    if hashlib.sha256(result).hexdigest() != patched:
        raise ValueError("Supabase rollout insertion changed")
    return result


def patch_chart(data, lock):
    if lock["version"] != "0.8.0" or lock["sha256"] != UPSTREAM_SHA or hashlib.sha256(data).hexdigest() != UPSTREAM_SHA:
        raise ValueError("Supabase chart does not match the reviewed upstream archive")
    output = io.BytesIO()
    seen = set()
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as source:
        # Stable gzip timestamp makes the derived archive reproducible.
        with gzip.GzipFile(fileobj=output, mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as target:
                for member in source:
                    content = source.extractfile(member).read() if member.isfile() else None
                    if member.name in TEMPLATES:
                        if member.name in seen or content is None:
                            raise ValueError("Duplicate or invalid Supabase deployment template")
                        seen.add(member.name)
                        content = patch_template(member.name, content)
                    metadata = copy.copy(member)
                    if content is not None:
                        metadata.size = len(content)
                    target.addfile(metadata, io.BytesIO(content) if content is not None else None)
    if seen != TEMPLATES.keys():
        raise ValueError("Pinned Supabase deployment templates missing")
    return output.getvalue()


if __name__ == "__main__":
    try:
        lock = json.loads((Path(__file__).with_name("chart.lock.json")).read_text())
        data = sys.stdin.buffer.read(20 * 1024 * 1024 + 1)
        if len(data) > 20 * 1024 * 1024:
            raise ValueError("Supabase archive exceeds size limit")
        sys.stdout.buffer.write(patch_chart(data, lock))
    except (ValueError, OSError, tarfile.TarError):
        sys.exit("Pinned Supabase rollout patch validation failed")

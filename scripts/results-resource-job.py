#!/usr/bin/env python3
"""Dry-run or stream one synthetic fixture to the deployed test image's offline harness."""
import argparse
import json
from pathlib import Path
import re
import subprocess

CONTEXT = "do-atl1-k8s-1-37-0-do-2-atl1-1793724241236"


def command(pod):
    if re.fullmatch(r"[a-z0-9][a-z0-9.-]{0,252}", pod) is None:
        raise ValueError("exact test pod name required")
    return ["kubectl", "--context", CONTEXT, "-n", "civpulse-test", "exec", "-i", pod, "-c", "collector", "--", "/usr/local/bin/civpulse-measure", "--scratch-root", "/tmp", "-", "2026-11-03", "--pipeline", "160", "4178"]


def verify(pod, digest):
    if re.fullmatch(r"sha256:[a-f0-9]{64}", digest) is None:
        raise ValueError("reviewed collector digest required")
    raw = subprocess.check_output(["kubectl", "--context", CONTEXT, "-n", "civpulse-test", "get", "pod", pod, "-o", "json"], timeout=30)
    obj = json.loads(raw)
    container = next(c for c in obj["spec"]["containers"] if c["name"] == "collector")
    status = next(c for c in obj["status"]["containerStatuses"] if c["name"] == "collector")
    if container["image"] != "registry.digitalocean.com/hatchk8s/civpulse-results-collector@" + digest or status["imageID"].split("@")[-1] != digest:
        raise ValueError("running image differs from reviewed collector digest")
    if not status.get("ready") or status.get("restartCount") != 0:
        raise ValueError("fresh ready zero-restart pod required")
    refs=container.get("envFrom", [])
    if len(refs)!=1 or "configMapRef" not in refs[0]:
        raise ValueError("reviewed collector ConfigMap required")
    name=refs[0]["configMapRef"]["name"]
    data=json.loads(subprocess.check_output(["kubectl", "--context", CONTEXT, "-n", "civpulse-test", "get", "configmap", name, "-o", "json"], timeout=30))["data"]
    if data.get("SOS_COLLECTOR_ENABLED")!="false" or any(e.get("name")=="SOS_COLLECTOR_ENABLED" and e.get("value")!="false" for e in container.get("env", [])):
        raise ValueError("collector must remain disabled during offline resource run")
    return container["resources"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pod", required=True)
    parser.add_argument("--image-digest", required=True)
    parser.add_argument("--fixture", required=True, type=Path)
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    argv = command(args.pod)
    if not args.apply:
        print(json.dumps({"command": argv, "fixture": str(args.fixture), "expected_image_digest": args.image_digest, "production_acceptance": False}))
        return
    resources = verify(args.pod, args.image_digest)
    print(json.dumps({"reviewed_image_digest": args.image_digest, "resources": resources, "fixture_bytes": args.fixture.stat().st_size}), flush=True)
    with args.fixture.open("rb") as fixture:
        subprocess.run(argv, stdin=fixture, check=True, timeout=900)


if __name__ == "__main__":
    main()

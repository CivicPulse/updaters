#!/usr/bin/env python3
"""Render or explicitly run one deployed-image alert delivery verification Job."""
import argparse
import json
import re
import subprocess
import sys

CONTEXT = "do-atl1-k8s-1-37-0-do-2-atl1-1793724241236"
NAME = "results-alert-route-verification"


def manifest(environment, digest, release="results"):
    if environment not in {"dev", "test", "prod"} or re.fullmatch(r"sha256:[a-f0-9]{64}", digest) is None or re.fullmatch(r"[a-z0-9][a-z0-9-]{0,52}", release) is None:
        raise ValueError("Explicit environment, immutable image digest and valid release are required")
    return {
        "apiVersion": "batch/v1", "kind": "Job",
        "metadata": {"name": NAME, "namespace": f"civpulse-{environment}"},
        "spec": {"backoffLimit": 0, "activeDeadlineSeconds": 90, "ttlSecondsAfterFinished": 86400, "template": {
            "metadata": {"labels": {"app.kubernetes.io/name": "civpulse-results-monitor", "app.kubernetes.io/instance": release}},
            "spec": {"automountServiceAccountToken": False, "restartPolicy": "Never", "securityContext": {"runAsNonRoot": True, "runAsUser": 65532, "runAsGroup": 65532, "seccompProfile": {"type": "RuntimeDefault"}},
                "imagePullSecrets": [{"name": "registry-hatchk8s"}],
                "containers": [{"name": "verify", "image": f"registry.digitalocean.com/hatchk8s/civpulse-results-collector@{digest}", "args": ["alert-test"],
                    "securityContext": {"allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True, "capabilities": {"drop": ["ALL"]}},
                    "resources": {"requests": {"cpu": "50m", "memory": "64Mi", "ephemeral-storage": "16Mi"}, "limits": {"cpu": "250m", "memory": "256Mi", "ephemeral-storage": "64Mi"}},
                    "env": [{"name": "CIVPULSE_ENV", "value": environment}, {"name": "SOS_COLLECTOR_ENABLED", "value": "false"}, {"name": "SOS_ALERTS_ENABLED", "value": "true"}, {"name": "CLOUDFLARE_EMAIL_API_TOKEN", "valueFrom": {"secretKeyRef": {"name": "results-alert-email", "key": "CLOUDFLARE_EMAIL_API_TOKEN"}}}]}]}}}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--environment", choices=["dev", "test", "prod"], default="test")
    parser.add_argument("--image-digest", required=True)
    parser.add_argument("--release", default="results")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    try:
        job = manifest(args.environment, args.image_digest, args.release)
        if not args.apply:
            print(json.dumps(job, indent=2))
            return 0
        command = ["kubectl", "--context", CONTEXT, "-n", f"civpulse-{args.environment}"]
        subprocess.run(command + ["apply", "-f", "-"], input=json.dumps(job).encode(), check=True)
        subprocess.run(command + ["wait", "--for=condition=complete", f"job/{NAME}", "--timeout=120s"], check=True)
        output = subprocess.check_output(command + ["logs", f"job/{NAME}"], timeout=30)
        receipt = json.loads(output)
        if receipt.get("delivery") != "delivered" or not receipt.get("message_id"):
            print("Email was not confirmed delivered; inspect provider evidence before acceptance", file=sys.stderr)
            return 1
        print(json.dumps({"environment": args.environment, "delivery": "delivered", "message_id": receipt["message_id"], "verification_job_collection_enabled": False}))
        return 0
    except (ValueError, OSError, subprocess.SubprocessError):
        print("Alert-route verification failed; collection remains disabled", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())

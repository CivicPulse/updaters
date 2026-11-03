#!/usr/bin/env python3
"""Render an isolated, credential-free offline resource/replay Job; never installs it."""
import argparse
import json
import re


def manifest(digest, raw_cap, scope_cap):
    if re.fullmatch(r"sha256:[a-f0-9]{64}", digest) is None:
        raise ValueError("immutable reviewed resource image digest required")
    if not 0 < scope_cap <= 67108864 or not scope_cap <= raw_cap <= 536870912:
        raise ValueError("caps must be positive and lower-only")
    labels = {"app.kubernetes.io/name": "civpulse-resource-acceptance"}
    return {"apiVersion": "v1", "kind": "List", "items": [
        {"apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy", "metadata": {"name": "civpulse-resource-acceptance", "namespace": "civpulse-test"}, "spec": {"podSelector": {"matchLabels": labels}, "policyTypes": ["Ingress", "Egress"], "ingress": [], "egress": []}},
        {"apiVersion": "batch/v1", "kind": "Job", "metadata": {"name": "civpulse-resource-acceptance", "namespace": "civpulse-test"}, "spec": {"backoffLimit": 0, "activeDeadlineSeconds": 900, "template": {"metadata": {"labels": labels}, "spec": {
            "restartPolicy": "Never", "automountServiceAccountToken": False, "securityContext": {"runAsNonRoot": True, "runAsUser": 65532, "runAsGroup": 65532, "fsGroup": 65532, "seccompProfile": {"type": "RuntimeDefault"}},
            "imagePullSecrets": [{"name": "registry-hatchk8s"}],
            "containers": [{"name": "measure", "image": "registry.digitalocean.com/hatchk8s/civpulse-results-resource@" + digest, "args": ["/fixtures/resource-case.json", "2026-11-03", "--pipeline", "160", "4178"],
                "securityContext": {"allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True, "capabilities": {"drop": ["ALL"]}},
                "env": [{"name": "SOS_MAX_DOWNLOAD_BYTES", "value": str(raw_cap)}, {"name": "SOS_MAX_SCOPE_BYTES", "value": str(scope_cap)}],
                "resources": {"requests": {"cpu": "100m", "memory": "512Mi", "ephemeral-storage": "2Gi"}, "limits": {"cpu": "1", "memory": "1Gi", "ephemeral-storage": "3Gi"}},
                "volumeMounts": [{"name": "scratch", "mountPath": "/tmp"}]}], "volumes": [{"name": "scratch", "emptyDir": {"sizeLimit": "2Gi"}}]}}}}]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image-digest", required=True)
    parser.add_argument("--raw-cap", type=int, default=536870912)
    parser.add_argument("--scope-cap", type=int, default=67108864)
    args = parser.parse_args()
    print(json.dumps(manifest(args.image_digest, args.raw_cap, args.scope_cap), indent=2))


if __name__ == "__main__":
    main()

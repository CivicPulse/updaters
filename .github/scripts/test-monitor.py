#!/usr/bin/env python3
"""Verify the monitor cannot become a source scheduler or expose credentials."""
from pathlib import Path
import subprocess
import yaml

ROOT = Path(__file__).resolve().parents[2]
CHART = ROOT / "deploy/helm/civpulse-results-collector"
DIGEST = "sha256:" + "a" * 64


def render(environment, enabled=True, alerts=True):
    output = subprocess.check_output([
        "helm", "template", "results", str(CHART), "--namespace", "civpulse-" + environment,
        "--set", "environment=" + environment, "--set-string", "image.digest=" + DIGEST,
        "--set", "monitoring.enabled=" + str(enabled).lower(),
        "--set", "monitoring.alertsEnabled=" + str(alerts).lower(),
    ], text=True)
    return list(yaml.safe_load_all(output))


def main():
    for environment in ["dev", "test", "prod"]:
        docs = render(environment)
        assert all(doc["metadata"]["namespace"] == "civpulse-" + environment for doc in docs)
        assert not any(doc["kind"] == "CronJob" for doc in docs)
        monitors = [doc for doc in docs if doc["kind"] == "Deployment" and doc["metadata"]["name"].endswith("-monitor")]
        assert len(monitors) == 1
        monitor = monitors[0]
        assert monitor["spec"]["replicas"] == 1
        assert monitor["spec"]["strategy"] == {"type": "Recreate"}
        pod = monitor["spec"]["template"]["spec"]
        assert pod["restartPolicy"] == "Always"
        assert pod["terminationGracePeriodSeconds"] == 110
        assert pod["automountServiceAccountToken"] is False
        assert pod["securityContext"]["runAsNonRoot"] is True
        container = pod["containers"][0]
        assert container["args"] == ["notify-daemon"]
        for probe in ["startupProbe", "readinessProbe", "livenessProbe"]:
            assert container[probe]["exec"]["command"] == ["/usr/local/bin/civpulse-tool", "notify-liveness"]
        state_volume = next(volume for volume in pod["volumes"] if "persistentVolumeClaim" in volume)
        assert state_volume["persistentVolumeClaim"]["claimName"] == monitor["metadata"]["name"]
        state_mount = next(mount for mount in container["volumeMounts"] if mount["name"] == state_volume["name"])
        assert state_mount["mountPath"] == "/state"
        assert state_mount.get("readOnly", False) is False
        assert container["image"] == "registry.digitalocean.com/hatchk8s/civpulse-results-collector@" + DIGEST
        assert container["securityContext"]["readOnlyRootFilesystem"] is True
        assert container["securityContext"]["capabilities"]["drop"] == ["ALL"]
        env = {entry["name"]: entry for entry in container["env"]}
        assert env["SOS_COLLECTOR_ENABLED"]["value"] == "false"
        assert env["SOS_MONITOR_STATE"]["value"].startswith("/state/")
        assert env["SOS_ALERT_STATE"]["value"].startswith("/state/")
        token = env["CLOUDFLARE_EMAIL_API_TOKEN"]
        assert "value" not in token
        assert token["valueFrom"]["secretKeyRef"] == {"name": "results-alert-email", "key": "CLOUDFLARE_EMAIL_API_TOKEN", "optional": False}
        pvc = next(doc for doc in docs if doc["kind"] == "PersistentVolumeClaim")
        assert pvc["metadata"]["annotations"]["helm.sh/resource-policy"] == "keep"
        assert pvc["spec"]["storageClassName"] == "do-block-storage-retain"
        policy = next(doc for doc in docs if doc["kind"] == "NetworkPolicy" and doc["metadata"]["name"].endswith("-monitor"))
        assert policy["spec"]["ingress"] == []
        assert all(port["port"] in {53, 443, 8000} for rule in policy["spec"]["egress"] for port in rule["ports"])
        assert not any(doc["kind"] in {"Service", "Ingress", "Role", "RoleBinding"} for doc in docs)
        assert not any(doc["kind"] == "Deployment" and doc["metadata"]["name"].endswith("-monitor") for doc in render(environment, enabled=False))
    base = ["helm", "template", "results", str(CHART), "--namespace", "civpulse-test", "--set", "environment=test", "--set-string", "image.digest=sha256:" + "a" * 64]
    for values in [("536870913", "67108864"), ("536870912", "67108865"), ("0", "1"), ("1", "2")]:
        result = subprocess.run(base + ["--set", "collection.maxDownloadBytes=" + values[0], "--set", "collection.maxScopeBytes=" + values[1]], capture_output=True)
        assert result.returncode != 0, "invalid admission caps rendered successfully"
    print("monitor isolation, fixed notification command, durable state and secret references verified")


if __name__ == "__main__":
    main()

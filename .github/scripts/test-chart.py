#!/usr/bin/env python3
"""Exercise chart safety invariants and fail-closed activation guards."""
import json
from pathlib import Path
import subprocess
import sys
import yaml

ROOT = Path(__file__).resolve().parents[2]
CHART = ROOT / "deploy/helm/civpulse-results-collector"
DIGEST = "sha256:" + "a" * 64


def render(environment="dev", overrides=(), success=True, namespace=None, digest=DIGEST):
    args = ["helm", "template", "results", str(CHART), "--namespace", namespace or "civpulse-"+environment,
            "-f", str(ROOT/f"deploy/environments/{environment}.yaml"), "--set-string", "image.digest="+digest]
    for entry in overrides:
        args.extend(["--set", entry])
    result = subprocess.run(args, capture_output=True, text=True)
    if (result.returncode == 0) != success:
        raise AssertionError("Unexpected chart result: " + result.stderr)
    return list(yaml.safe_load_all(result.stdout)) if success else []


def main():
    for environment in ["dev", "test", "prod"]:
        docs = render(environment)
        assert {doc["kind"] for doc in docs} == {"Deployment", "ConfigMap", "NetworkPolicy"}
        assert all(doc["metadata"]["namespace"] == "civpulse-"+environment for doc in docs)
        deployment = next(doc for doc in docs if doc["kind"] == "Deployment")
        config = next(doc["data"] for doc in docs if doc["kind"] == "ConfigMap")
        assert config["CIVPULSE_ENV"] == environment
        assert config["SOS_COLLECTOR_ENABLED"] == "false"
        assert config["SOS_MAX_DOWNLOAD_BYTES"] == "134217728"
        assert config["SOS_MAX_SCOPE_BYTES"] == "16777216"
        assert config["R2_BUCKET"] == "civpulse-files" + ("" if environment == "prod" else "-"+environment)
        assert config["R2_PUBLIC_ORIGIN"] == "https://files" + ("" if environment == "prod" else "-"+environment) + ".civpulse.org"
        pod = deployment["spec"]["template"]["spec"]
        container = pod["containers"][0]
        assert deployment["spec"]["replicas"] == 1
        assert pod["automountServiceAccountToken"] is False
        assert pod["securityContext"]["runAsNonRoot"] is True
        assert pod["securityContext"]["seccompProfile"]["type"] == "RuntimeDefault"
        assert container["image"] == "registry.digitalocean.com/hatchk8s/civpulse-results-collector@"+DIGEST
        assert container["args"] == ["daemon"]
        assert container["resources"]["requests"]["memory"] == "512Mi"
        assert container["resources"]["limits"]["memory"] == "1Gi"
        assert container["resources"]["limits"]["cpu"] == "1"
        assert config["SOS_COLLECTOR_CONCURRENCY"] == "1"
        assert container["securityContext"]["readOnlyRootFilesystem"] is True
        assert container["securityContext"]["allowPrivilegeEscalation"] is False
        assert container["securityContext"]["capabilities"]["drop"] == ["ALL"]
        assert pod["volumes"] == [{"name":"scratch","emptyDir":{"sizeLimit":"2Gi"}}]
        assert all(container[p]["exec"]["command"] == ["/usr/local/bin/civpulse-tool", "liveness"]
                   for p in ("startupProbe", "livenessProbe", "readinessProbe"))
        assert all(ref["valueFrom"]["secretKeyRef"]["optional"] for ref in container["env"])
        assert len(container["envFrom"]) == 1
        assert {entry["name"] for entry in container["env"]} == {"SUPABASE_URL", "SUPABASE_SERVICE_ROLE_KEY", "R2_ENDPOINT", "R2_ACCESS_KEY_ID", "R2_SECRET_ACCESS_KEY"}
        policy = next(doc["spec"] for doc in docs if doc["kind"] == "NetworkPolicy")
        assert policy["ingress"] == []
        if environment == "prod":
            assert len(policy["egress"]) == 2
            assert config["SOS_ELECTION_ID"] == "295d864d-80c8-4a87-ab16-3f4f3fa5ddb7"
            assert config["SOS_ELECTION_DATE"] == "2026-11-03"
            assert config["SOS_SOURCE_URL"] == "https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json"
            assert config["SOS_EXPECTED_JURISDICTIONS"] == "160"
            assert config["SOS_EXPECTED_ITEMS"] == "4178"
            monitor = render("prod", ["monitoring.enabled=true"])
            cron = next(doc for doc in monitor if doc["kind"] == "CronJob")
            monitor_env = {entry["name"]: entry.get("value") for entry in cron["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["env"]}
            assert monitor_env["SOS_POLL_INITIAL_INTERVAL_SECONDS"] == "3600"
            assert monitor_env["SOS_POLL_INTERVAL_SECONDS"] == "300"
            assert monitor_env["SOS_POLL_TRANSITION_AT"] == "2026-11-03T22:00:00Z"
        # Database health must not be a liveness/readiness restart dependency.
        assert "health" not in json.dumps(container)
    for environment in ["dev", "test", "prod"]:
        docs = render(environment, ["monitoring.enabled=true"])
        cron = next(doc for doc in docs if doc["kind"] == "CronJob")
        env = {item["name"]: item.get("value") for item in cron["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["env"]}
        assert env["SOS_NTFY_ENABLED"] == str(environment == "prod").lower()
        assert env["SOS_NTFY_URL"] == "https://ntfy.sh/CivPulse"
        assert env["SOS_NTFY_STATE"] == "/state/ntfy.json"
        assert env["SOS_ALERT_STATE"] != env["SOS_NTFY_STATE"]
    render("dev", ["monitoring.ntfyEnabled=true"], success=False)
    render("prod", ["monitoring.ntfyUrl=https://unreviewed.example/private"], success=False)
    reviewed = ["collection.enabled=true", "collection.electionId=00000000-0000-4000-8000-000000000001",
                "collection.electionDate=2026-11-03",
                "collection.sourceUrl=https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json",
                "collection.expectedItems=4178"]
    render("dev", ["collection.enabled=true"], success=False)
    render("prod", reviewed, success=False)
    render("prod", reviewed+["acceptance.approved=true"], success=False)
    enabled = render("prod", reviewed+["acceptance.approved=true", "acceptance.reference=https://github.com/CivicPulse/updaters/issues/9"])
    pod = next(x for x in enabled if x["kind"] == "Deployment")["spec"]["template"]["spec"]
    assert all(ref["valueFrom"]["secretKeyRef"]["optional"] is False for ref in pod["containers"][0]["env"])
    render(namespace="default", success=False)
    render(digest="latest", success=False)
    render(digest="", success=False)
    render(overrides=["collection.concurrency=5"], success=False)
    render(overrides=["resources.limits.memory=unbounded"], success=False)
    render(overrides=["image.repository=docker.io/unreviewed/worker"], success=False)
    for environment in ["dev", "test", "prod"]:
        subprocess.run(["helm", "lint", str(CHART), "--strict", "--namespace", "civpulse-"+environment,
                        "-f", str(ROOT/f"deploy/environments/{environment}.yaml"), "--set-string", "image.digest="+DIGEST], check=True)
    print("Collector chart safety and activation guards passed")


if __name__ == "__main__":
    main()

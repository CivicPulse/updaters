#!/usr/bin/env python3
"""Check API authentication boundaries and production ingress availability."""
from pathlib import Path
import sys
import yaml

ROOT = Path(__file__).resolve().parents[2]
documents = []
for name in ("deploy/k8s/webapi.yaml", "deploy/edge/cloudflared.yaml"):
    documents.extend(yaml.safe_load_all((ROOT / name).read_text()))
documents.extend(yaml.safe_load_all(Path(sys.argv[1]).read_text()))
services = [d for d in documents if d and d["kind"] == "Service"]
assert len(services) == 2
for service in services:
    assert service["spec"]["type"] == "ClusterIP"
    assert all("nodePort" not in port for port in service["spec"]["ports"])
    assert [port["port"] for port in service["spec"]["ports"]] == [80]
deployments = [d for d in documents if d and d["kind"] == "Deployment"]
assert {d["metadata"]["name"] for d in deployments} == {"civpulse-webapi", "cloudflared", "traefik"}
for deployment in deployments:
    assert deployment["spec"]["replicas"] == 2
    spec = deployment["spec"]["template"]["spec"]
    assert not spec.get("hostNetwork", False)
    assert not any("hostPort" in port for c in spec["containers"] for port in c.get("ports", []))
    rules = spec["affinity"]["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"]
    assert len(rules) == 1 and rules[0]["topologyKey"] == "kubernetes.io/hostname"
    selector = rules[0]["labelSelector"]["matchLabels"]
    labels = deployment["spec"]["template"]["metadata"]["labels"]
    assert selector and all(labels.get(key) == value for key, value in selector.items())
    if deployment["metadata"]["name"] == "civpulse-webapi":
        # Kubernetes probes must not introduce unauthenticated HTTP API endpoints.
        container = spec["containers"][0]
        assert container["readinessProbe"] == {"tcpSocket": {"port": "http"}}
        assert container["livenessProbe"]["tcpSocket"] == {"port": "http"}
        env = {item["name"]: item for item in container["env"]}
        assert env["SUPABASE_PUBLISHABLE_KEY"]["valueFrom"]["secretKeyRef"]["name"] == "webapi-supabase"
        assert "SUPABASE_SERVICE_ROLE_KEY" not in env
budgets = [d for d in documents if d and d["kind"] == "PodDisruptionBudget"]
assert {d["metadata"]["name"] for d in budgets} == {"civpulse-webapi", "cloudflared", "traefik"}
assert all(d["spec"]["minAvailable"] == 1 for d in budgets)
ingress = next(d for d in documents if d and d["kind"] == "Ingress")
assert ingress["spec"]["ingressClassName"] == "civpulse-internal"
rule = ingress["spec"]["rules"][0]
assert rule["host"] == "webapi.civpulse.org"
assert len(rule["http"]["paths"]) == 1
assert rule["http"]["paths"][0]["path"] == "/api/v1"
print("API manifests: internal services, owned route, authenticated probes, spread replicas and disruption budgets passed")

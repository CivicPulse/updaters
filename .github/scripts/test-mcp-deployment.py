#!/usr/bin/env python3
"""Check MCP route ownership and the production workload's isolation contract."""
import json
from pathlib import Path
import yaml

root = Path(__file__).resolve().parents[2]
documents = list(yaml.safe_load_all((root / "deploy/k8s/mcp-production.yaml").read_text()))
by_kind = {d["kind"]: d for d in documents}
assert set(by_kind) == {"Deployment", "Service", "NetworkPolicy", "PodDisruptionBudget"}
assert all(d["metadata"]["namespace"] == "civpulse-prod" for d in documents)
deployment = by_kind["Deployment"]
assert deployment["spec"]["replicas"] == 2
assert deployment["spec"]["strategy"]["rollingUpdate"] == {"maxUnavailable": 0, "maxSurge": 1}
pod = deployment["spec"]["template"]["spec"]
assert pod["automountServiceAccountToken"] is False
assert pod["securityContext"]["runAsNonRoot"] is True
spread = pod["affinity"]["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"]
assert spread[0]["topologyKey"] == "kubernetes.io/hostname"
container = pod["containers"][0]
receipt = json.loads((root / "deploy/k8s/mcp-release.json").read_text())
assert container["image"] == receipt["image"]
assert "@sha256:" in container["image"]
assert container["securityContext"]["readOnlyRootFilesystem"] is True
assert container["securityContext"]["allowPrivilegeEscalation"] is False
assert container["securityContext"]["capabilities"]["drop"] == ["ALL"]
env = {item["name"]: item for item in container["env"]}
assert env["MCP_RESOURCE_URL"]["value"] == "https://webapi.civpulse.org/mcp/v2"
assert env["SUPABASE_ANON_KEY"]["valueFrom"]["secretKeyRef"] == {"name": "webapi-supabase", "key": "publishable-key"}
assert "SUPABASE_SERVICE_ROLE_KEY" not in env
service = by_kind["Service"]["spec"]
assert service["type"] == "ClusterIP"
assert service["selector"] == deployment["spec"]["selector"]["matchLabels"]
assert service["ports"] == [{"name": "http", "port": 8789, "targetPort": "http"}]
policy = by_kind["NetworkPolicy"]["spec"]
assert policy["podSelector"]["matchLabels"] == service["selector"]
source = policy["ingress"][0]["from"][0]
assert source["namespaceSelector"]["matchLabels"] == {"kubernetes.io/metadata.name": "civpulse-edge"}
assert source["podSelector"]["matchLabels"] == {"app.kubernetes.io/name": "traefik"}
assert by_kind["PodDisruptionBudget"]["spec"]["minAvailable"] == 1
ingress = yaml.safe_load((root / "deploy/k8s/mcp-ingress.yaml").read_text())
assert ingress["spec"]["ingressClassName"] == "civpulse-internal"
assert ingress["metadata"]["annotations"]["traefik.ingress.kubernetes.io/router.entrypoints"] == "web"
rule = ingress["spec"]["rules"][0]
assert rule["host"] == "webapi.civpulse.org"
paths = rule["http"]["paths"]
assert {p["path"] for p in paths} == {"/mcp/v2", "/functions/v1/mcp/v2", "/.well-known/oauth-protected-resource"}
assert all(p["pathType"] == "Prefix" for p in paths)
assert all(p["backend"]["service"] == {"name": "civpulse-mcp", "port": {"name": "http"}} for p in paths)
assert not any("middleware" in key or "retry" in key for key in ingress["metadata"]["annotations"])
api = next(d for d in yaml.safe_load_all((root / "deploy/k8s/webapi.yaml").read_text()) if d["kind"] == "Ingress")
assert api["spec"]["rules"][0]["http"]["paths"][0]["path"] == "/api/v1"
print("MCP deployment: isolated internal service, immutable image, spread replicas and distinct Traefik paths passed")

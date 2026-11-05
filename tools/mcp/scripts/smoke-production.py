#!/usr/bin/env python3
"""Bounded read-only checks. Supply a normal user's MCP_SMOKE_TOKEN in the environment."""
import argparse
import json
import os
from pathlib import Path
import urllib.error
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="https://webapi.civpulse.org")
    parser.add_argument("--resource", default="https://webapi.civpulse.org/mcp/v2")
    parser.add_argument("--skip-api", action="store_true", help="For candidate port-forward only")
    args = parser.parse_args()
    opener = urllib.request.build_opener(NoRedirect())
    checks = []

    def request(path, method="GET", body=None, token=None, origin=None):
        headers = {"User-Agent": "curl/8.18.0", "Accept": "application/json, text/event-stream"}
        if token:
            headers["Authorization"] = "Bearer " + token
        if origin:
            headers["Origin"] = origin
        data = None if body is None else json.dumps(body).encode()
        if data is not None:
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(args.base.rstrip("/") + path, data=data, headers=headers, method=method)
        try:
            response = opener.open(req, timeout=65)
        except urllib.error.HTTPError as error:
            response = error
        raw = response.read()
        try:
            body = json.loads(raw)
        except (ValueError, UnicodeDecodeError):
            body = None
        return response.status, response.headers, body

    for path in ["/mcp/v2/.well-known/oauth-protected-resource", "/.well-known/oauth-protected-resource/mcp/v2", "/.well-known/oauth-protected-resource"]:
        status, headers, body = request(path)
        assert status == 200, (path, status)
        assert body["resource"] == args.resource
        assert body["authorization_servers"] == ["https://drzcrodqflzkzzbqsags.supabase.co/auth/v1"]
        assert headers.get("Cache-Control") == "no-store"
    checks.append("oauth_metadata_and_issuer")
    for path in ["/mcp/v2", "/mcp/v2/", "/functions/v1/mcp/v2"]:
        status, headers, _ = request(path, "POST", {})
        assert status == 401, (path, status)
        assert args.resource + "/.well-known/oauth-protected-resource" in headers.get("WWW-Authenticate", "")
        assert headers.get("Cache-Control") == "no-store"
    status, _, _ = request("/mcp/v2", "POST", {}, "invalid-test-token")
    assert status == 401
    checks.append("authentication_challenges")
    expected_health_status = 200 if args.skip_api else 404
    assert request("/healthz")[0] == expected_health_status
    checks.append("health_not_public" if not args.skip_api else "candidate_health")
    status, headers, _ = request("/mcp/v2", "OPTIONS", origin="https://www.civpulse.org")
    assert status == 204
    assert headers.get("Access-Control-Allow-Origin") == "https://www.civpulse.org"
    checks.append("browser_preflight")
    if not args.skip_api:
        status, headers, _ = request("/api/v1/me")
        assert status == 401
        assert headers.get("Cache-Control") == "no-store"
        checks.append("api_auth_route_preserved")

    token = os.environ.get("MCP_SMOKE_TOKEN")
    if token:
        def rpc(method, params):
            status, headers, body = request("/mcp/v2", "POST", {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}, token)
            assert status == 200, (method, status)
            assert headers.get("Cache-Control") == "no-store"
            assert "error" not in body, (method, body.get("error"))
            return body["result"]

        def tool(name, arguments):
            result = rpc("tools/call", {"name": name, "arguments": arguments})
            assert not result.get("isError"), (name, result)
            return json.loads(result["content"][0]["text"])

        assert rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "deployment-smoke", "version": "1"}})["protocolVersion"] == "2025-11-25"
        expected = json.loads((Path(__file__).resolve().parents[3] / "crates/civpulse-civic/reference/tools.json").read_text())
        expected += json.loads((Path(__file__).resolve().parents[3] / "crates/civpulse-civic/reference/extensions.json").read_text())
        assert rpc("tools/list", {})["tools"] == expected
        checks.append(f"authenticated_full_catalog_{len(expected)}")
        identity = tool("whoami", {})
        assert identity["is_admin"] is False
        if os.environ.get("MCP_SMOKE_USER_ID"):
            assert identity["user_id"] == os.environ["MCP_SMOKE_USER_ID"]
        checks.append("ordinary_user_identity")
        tool("search_site", {"query": "Macon", "limit": 1})
        checks.append("live_rls_civic_read")
        denied = rpc("tools/call", {"name": "search_users", "arguments": {"query": "deployment-smoke-no-match"}})
        assert denied.get("isError") is True
        assert "Admin access required" in denied["content"][0]["text"]
        checks.append("ordinary_user_admin_read_rejected")
        if not args.skip_api:
            status, headers, _ = request("/api/v1/me", token=token)
            assert status == 200
            assert headers.get("Cache-Control") == "no-store"
            checks.append("existing_api_accepts_same_user_token")
    print(json.dumps({"passed": True, "base": args.base, "resource": args.resource, "checks": checks}, indent=2))


if __name__ == "__main__":
    main()

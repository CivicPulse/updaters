#!/usr/bin/env python3
"""Provision isolated dev/test Supabase; never accepts production or another cluster.

No secrets appear in argv, Helm values, logs, or source. Downloads and migrations
are verified against checked-in SHA-256 locks. Replays only missing migrations.
"""
import argparse
import base64
import concurrent.futures
import hashlib
import hmac
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import tempfile
import time
import urllib.request

CONTEXT = "thor"
HERE = Path(__file__).resolve().parent


def run(args, data=None, capture=True):
    # Dependency errors may contain credentials; never print captured output.
    environment = os.environ.copy()
    if args[0] == "kubectl" and "exec" in args and "-i" in args:
        # Thor k3s 1.34 can leave stdin open with newer kubectl WebSocket exec.
        # SPDY forwards EOF correctly when replaying SQL through psql stdin.
        environment.setdefault("KUBECTL_REMOTE_COMMAND_WEBSOCKETS", "false")
    result = subprocess.run(args, input=data, stdout=subprocess.PIPE if capture else None,
                            stderr=subprocess.PIPE, check=False, env=environment)
    if result.returncode:
        raise RuntimeError("Command failed: " + args[0] + " " + args[1] + " (inspect privately)")
    return result.stdout or b""


def b64(value):
    return base64.urlsafe_b64encode(value).rstrip(b"=").decode()


def jwt(secret, role):
    now = int(time.time())
    header = b64(b'{"alg":"HS256","typ":"JWT"}')
    payload = b64(json.dumps({"role":role,"iss":"civpulse-isolated", "iat":now,
                             "exp":now+365*86400}, separators=(",",":")).encode())
    message = header + "." + payload
    return message + "." + b64(hmac.new(secret.encode(), message.encode(), hashlib.sha256).digest())


def download(url, expected):
    with urllib.request.urlopen(url, timeout=120) as response:
        data = response.read(20 * 1024 * 1024 + 1)
    if len(data) > 20 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != expected:
        raise RuntimeError("Pinned download checksum/size mismatch")
    return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("environment", choices=["dev", "test"])
    parser.add_argument("--context", default=CONTEXT, choices=[CONTEXT])
    parser.add_argument("--apply", action="store_true", help="Explicitly create isolated resources and replay migrations")
    args = parser.parse_args()
    ns = "civpulse-" + args.environment
    print("Target:", CONTEXT, ns, "Supabase chart 0.8.0; collection remains disabled")
    if not args.apply:
        print("Dry run. Pass --apply after reviewing the chart and bootstrap.")
        return
    # Verify and derive the reviewed chart before any cluster/secret writes.
    lock = json.loads((HERE/"chart.lock.json").read_text())
    chart_bytes = run([sys.executable, str(HERE/"patch-chart.py")], download(lock["url"], lock["sha256"]))
    kube = ["kubectl", "--context", args.context, "-n", ns]
    cluster = json.loads(run(["kubectl", "--context", args.context, "config", "view", "--minify", "-o", "json"]))
    if cluster["contexts"][0]["context"]["cluster"] != CONTEXT:
        raise RuntimeError("Unexpected cluster mapping")
    run(kube + ["get", "namespace", ns])

    def existing(name):
        result = run(kube + ["get", "secret", name, "--ignore-not-found", "-o", "json"])
        return json.loads(result) if result.strip() else None

    def secret(name, fields):
        prior = existing(name)
        if prior:
            missing = set(fields) - set(prior.get("data", {}))
            if missing:
                raise RuntimeError("Existing secret has missing required keys: " + name)
            return {key:base64.b64decode(value).decode() for key,value in prior["data"].items()}
        document = {"apiVersion":"v1","kind":"Secret","metadata":{"name":name,"namespace":ns,
                    "labels":{"app.kubernetes.io/part-of":"civpulse","civpulse.org/environment":args.environment}},
                    "type":"Opaque","stringData":fields}
        run(kube + ["create", "-f", "-"], json.dumps(document).encode())
        return fields

    signing = secrets.token_hex(32)
    token = secret("supabase-jwt", {"secret":signing,"anonKey":jwt(signing,"anon"),"serviceKey":jwt(signing,"service_role")})
    password = secrets.token_hex(32)
    secret("supabase-db", {"password":password,"password_encoded":password,"database":"postgres"})
    other = {
        "analytics":{"publicAccessToken":secrets.token_hex(32),"privateAccessToken":secrets.token_hex(32)},
        "bigquery":{"gcloud.json":"{}"},
        "smtp":{"username":secrets.token_hex(16),"password":secrets.token_hex(32)},
        "dashboard":{"username":"operator","password":secrets.token_hex(32),"openAiApiKey":""},
        "s3":{"keyId":secrets.token_hex(16),"accessKey":secrets.token_hex(32)},
        "realtime":{"secretKeyBase":secrets.token_hex(64),"dbEncKey":secrets.token_hex(8)},
        "meta":{"cryptoKey":secrets.token_hex(32)},
        "minio":{"user":secrets.token_hex(16),"password":secrets.token_hex(32)},
    }
    for name, fields in other.items():
        secret("supabase-"+name, fields)
    collector_credentials = {"SUPABASE_URL":f"http://supabase-supabase-kong.{ns}.svc.cluster.local:8000",
                             "SUPABASE_SERVICE_ROLE_KEY":token["serviceKey"]}
    actual_credentials = secret("results-supabase", collector_credentials)
    if any(actual_credentials[key] != value for key,value in collector_credentials.items()):
        raise RuntimeError("Existing collector database secret does not match this isolated Supabase")
    manifest = json.loads((HERE/"migrations.lock.json").read_text())
    with tempfile.TemporaryDirectory(prefix="civpulse-supabase-") as directory:
        os.chmod(directory, 0o700)
        directory = Path(directory)
        chart = directory/"supabase.tgz"
        chart.write_bytes(chart_bytes)
        run(["helm", "upgrade", "--install", "supabase", str(chart), "--kube-context", args.context,
             "--namespace", ns, "-f", str(HERE/"values.yaml"), "--wait", "--timeout", "15m"])
        # Auth and Storage services apply their own schemas before application migrations.
        for name in ["auth","storage","rest","kong"]:
            run(kube+["rollout","status",f"deployment/supabase-supabase-{name}","--timeout=300s"])
        def fetch(item):
            name, checksum = item
            url = f'https://raw.githubusercontent.com/{manifest["repository"]}/{manifest["commit"]}/supabase/migrations/{name}'
            return name, download(url, checksum)
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            migrations = dict(pool.map(fetch, manifest["files"].items()))
        psql = kube+["exec","-i","supabase-supabase-db-0","-c","supabase-db","--",
                     "psql","-X","-U","postgres","-d","postgres","-v","ON_ERROR_STOP=1","-qAt"]
        # These are standard Supabase baseline extensions, not application SQL.
        run(psql, b"create schema if not exists extensions; create extension if not exists pgcrypto with schema extensions; create extension if not exists supabase_vault;\n")
        checks = run(psql, b"select (to_regclass('auth.users') is not null and to_regclass('storage.buckets') is not null and to_regclass('storage.objects') is not null and to_regclass('vault.decrypted_secrets') is not null)::text;\n").strip()
        if checks != b"true":
            raise RuntimeError("Supabase schema initialization incomplete")
        run(psql, b"create schema if not exists civpulse_dev_bootstrap; create table if not exists civpulse_dev_bootstrap.migrations(name text primary key, sha256 text not null); revoke all on schema civpulse_dev_bootstrap from public;\n")
        applied = {}
        for line in run(psql, b"select name || '|' || sha256 from civpulse_dev_bootstrap.migrations order by name;\n").decode().splitlines():
            name, checksum = line.split("|")
            applied[name] = checksum
        for name, checksum in manifest["files"].items():
            if name in applied:
                if applied[name] != checksum:
                    raise RuntimeError("Applied migration hash conflict: " + name)
                continue
            # Tracking row and SQL commit together; never claim success after a partial file.
            body = b"BEGIN;\n" + migrations[name] + f"\nINSERT INTO civpulse_dev_bootstrap.migrations VALUES ('{name}','{checksum}');\nCOMMIT;\n".encode()
            run(psql, body)
            print("Applied:", name)
        acceptance = run(psql, b"select not exists(select 1 from public.sos_automation_feeds where enabled); select not exists(select 1 from vault.secrets where name in ('sos_results_poll_url','sos_results_poll_token','frontend_rebuild_url','frontend_rebuild_token')); select public.get_sos_collection_health() is not null;\n").decode().splitlines()
        if acceptance != ["t","t","t"]:
            raise RuntimeError("Disabled-feed/no-external-Vault acceptance failed")
        run(psql, b"NOTIFY pgrst, 'reload schema';\n")
        print("Verified isolated application schema; feeds disabled; no external poll/rebuild secrets.")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as error:
        # Do not print exception chains, request URLs or command output.
        raise SystemExit(str(error) if isinstance(error, RuntimeError) else "Bootstrap failed; inspect dependencies privately") from None

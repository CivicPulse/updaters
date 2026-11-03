#!/usr/bin/env python3
"""Plan existing admin RPCs; execute only with --apply and administrator credentials."""
import argparse
import datetime as dt
import json
import os
from pathlib import Path
import subprocess
import sys
from urllib.parse import urlsplit
import uuid

SOURCE = "https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json"
STOP_AT = "2026-11-11T00:00:00Z"
TRANSITION_AT = "2026-11-03T22:00:00Z"
ACCEPTANCE_CHECKS = {
    "contracts", "resource_envelope", "main_first_migrations", "isolated_e2e",
    "production_secret_isolation", "production_r2_checksums", "production_r2_conditional_writes",
    "production_cors_cache", "ballot_live_readback", "monitoring_alert_route",
    "recovery_drills", "cutover_rollback", "operator_review",
}


def instant(value):
    if not isinstance(value, str):
        raise ValueError("Polling timestamps must be strings")
    parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("Polling window timestamps must include a timezone")
    return parsed


def cadence(args):
    production = args.environment == "prod"
    initial = getattr(args, "initial_interval_seconds", None)
    after = getattr(args, "interval_seconds", None)
    initial = (3600 if production else 300) if initial is None else initial
    after = 300 if after is None else after
    transition = getattr(args, "transition_at", None) or (TRANSITION_AT if production else None)
    if any(value < 300 or value > 86400 or value % 300 for value in [initial, after]):
        raise ValueError("Coordinator intervals must be five-minute multiples in 300..86400")
    if transition:
        instant(transition)
    if production and (initial != 3600 or after != 300 or transition != TRANSITION_AT):
        raise ValueError("Production cadence changes require a separately reviewed rollout")
    return {"p_election_id": str(uuid.UUID(args.election_id)), "p_interval_seconds": initial, "p_transition_at": transition, "p_after_interval_seconds": after}


def plan(args):
    election = str(uuid.UUID(args.election_id))
    if args.action == "status":
        return "get_sos_results_health", {"p_election_id": election}
    if args.action == "pause":
        return "configure_sos_results_feed", {"p_election_id": election, "p_enabled": False}
    if args.action in {"retry", "pointer-repair", "abandon"}:
        return "recover_sos_results_job", {
            "p_election_id": election,
            "p_action": "abandon" if args.action == "abandon" else "retry",
        }
    if not args.starts_at or not args.ends_at or args.expected_source_url != SOURCE:
        raise ValueError("Resume requires the exact reviewed SOS URL and explicit polling window")
    if instant(args.starts_at) >= instant(args.ends_at):
        raise ValueError("Polling start must precede end")
    approved_cadence = cadence(args)
    if args.environment == "prod" and instant(args.ends_at) != instant(STOP_AT):
        raise ValueError("Production stop-window extension requires review")
    if args.action == "window":
        return "configure_sos_results_feed_window", {"p_election_id": election, "p_starts_at": args.starts_at, "p_ends_at": args.ends_at}
    if args.environment == "prod":
        if not args.acceptance_record:
            raise ValueError("Production resume requires a completed acceptance record")
        record = json.loads(Path(args.acceptance_record).read_text())
        if not isinstance(record, dict):
            raise ValueError("Acceptance record must be an object")
        if not (
            record.get("accepted") is True
            and record.get("environment") == "prod"
            and record.get("election_id") == election
            and record.get("source_url") == SOURCE
            and record.get("election_date") == "2026-11-03"
            and record.get("initial_poll_interval_seconds") == approved_cadence["p_interval_seconds"]
            and record.get("poll_transition_at") == approved_cadence["p_transition_at"]
            and record.get("poll_interval_seconds") == approved_cadence["p_after_interval_seconds"]
            and record.get("starts_at") == args.starts_at
            and record.get("ends_at") == args.ends_at
            and isinstance(record.get("operator"), str)
            and record["operator"].strip()
            and isinstance(record.get("checks"), dict)
            and ACCEPTANCE_CHECKS.issubset(record["checks"])
            and all(record["checks"][name] is True for name in ACCEPTANCE_CHECKS)
        ):
            raise ValueError("Production acceptance identity/window/operator/checks are incomplete")
    return "configure_sos_results_feed", {
        "p_election_id": election,
        "p_enabled": True,
        "p_starts_at": args.starts_at,
        "p_ends_at": args.ends_at,
    }


def verify_window(configured, args, election):
    if not isinstance(configured, dict) or configured.get("election_id") != election or configured.get("enabled") is not False or configured.get("source_url") != SOURCE or instant(configured.get("starts_at")) != instant(args.starts_at) or instant(configured.get("ends_at")) != instant(args.ends_at):
        raise ValueError("Paused window persistence mismatch; feed remains paused")


def target(environment, origin):
    parsed = urlsplit(origin)
    loopback = parsed.hostname in {"localhost", "127.0.0.1", "::1"}
    if (
        not parsed.hostname
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
        or parsed.path not in {"", "/"}
        or not (parsed.scheme == "https" or environment != "prod" and loopback and parsed.scheme == "http")
    ):
        raise ValueError("Admin RPC target requires a credential-free HTTPS origin (dev/test loopback HTTP allowed)")
    return origin.rstrip("/")


def execute(environment, rpc, payload):
    declared = os.environ.get("CIVPULSE_ENV")
    if declared != environment:
        raise ValueError("CIVPULSE_ENV must match the explicitly selected environment")
    origin = target(environment, os.environ["SUPABASE_URL"])
    key = os.environ["SUPABASE_ANON_KEY"]
    token = os.environ["SUPABASE_ACCESS_TOKEN"]
    if any(not value or "\r" in value or "\n" in value for value in (key, token)):
        raise ValueError("Invalid administrator credentials")
    # Secrets enter curl only through stdin, never shell interpolation/process arguments.
    options = {
        "url": f"{origin}/rest/v1/rpc/{rpc}",
        "request": "POST",
        "header": [f"apikey: {key}", f"Authorization: Bearer {token}", "Content-Type: application/json"],
        "data-binary": json.dumps(payload, separators=(",", ":")),
    }
    config = "\n".join(
        f"{name} = {json.dumps(value)}"
        for name, values in options.items()
        for value in (values if isinstance(values, list) else [values])
    )
    result = subprocess.run(
        ["curl", "--config", "-", "--silent", "--show-error", "--fail", "--max-time", "60", "--max-filesize", "1048576", "--max-redirs", "0", "--write-out", "\n%{http_code}"],
        input=config.encode(), capture_output=True, timeout=65, check=False,
    )
    if result.returncode:
        raise ValueError("Admin RPC failed or its outcome is unknown; read status before retrying")
    body, separator, status = result.stdout.rpartition(b"\n")
    if not separator or not status.isdigit() or not 200 <= int(status) < 300:
        raise ValueError("Admin RPC returned a non-success status")
    return json.loads(body or "null")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--environment", choices=["dev", "test", "prod"], required=True)
    parser.add_argument("--action", choices=["status", "pause", "window", "resume", "retry", "pointer-repair", "abandon"], required=True)
    parser.add_argument("--election-id", required=True)
    parser.add_argument("--starts-at")
    parser.add_argument("--ends-at")
    parser.add_argument("--expected-source-url")
    parser.add_argument("--acceptance-record")
    parser.add_argument("--initial-interval-seconds", type=int)
    parser.add_argument("--transition-at")
    parser.add_argument("--interval-seconds", type=int)
    parser.add_argument("--apply", action="store_true", help="Execute the RPC using explicit administrator credentials; otherwise print the plan")
    args = parser.parse_args()
    try:
        rpc, payload = plan(args)
        if args.apply:
            if args.action in {"resume", "window"}:
                current = execute(args.environment, "get_sos_results_health", {"p_election_id": payload["p_election_id"]})
                if not isinstance(current, dict) or current.get("enabled") is True:
                    raise ValueError("Pause the feed before configuring a reviewed resume")
                preflight = execute(args.environment, "configure_sos_results_feed", {"p_election_id": payload["p_election_id"], "p_enabled": False})
                if not isinstance(preflight, dict) or preflight.get("election_id") != payload["p_election_id"] or preflight.get("source_url") != SOURCE:
                    raise ValueError("Reviewed stored source URL differs; feed remains paused")
                if args.action == "resume":
                    execute(args.environment, "configure_sos_results_poll_cadence", cadence(args))
                    configured = execute(args.environment, "configure_sos_results_feed_window", {"p_election_id": payload["p_election_id"], "p_starts_at": args.starts_at, "p_ends_at": args.ends_at})
                    verify_window(configured, args, payload["p_election_id"])
            response = execute(args.environment, rpc, payload)
            if args.action == "window":
                verify_window(response, args, payload["p_election_id"])
            print(json.dumps(response, separators=(",", ":")))
        else:
            request = {"environment": args.environment, "rpc": rpc, "arguments": payload, "applied": False}
            if args.action in {"resume", "window"}:
                request["status_precondition"] = {"rpc": "get_sos_results_health", "arguments": {"p_election_id": payload["p_election_id"]}, "require_enabled": False}
                if args.action == "resume":
                    request["cadence"] = {"rpc": "configure_sos_results_poll_cadence", "arguments": cadence(args)}
                    request["window"] = {"rpc": "configure_sos_results_feed_window", "arguments": {"p_election_id": payload["p_election_id"], "p_starts_at": args.starts_at, "p_ends_at": args.ends_at}, "require_enabled": False}
                request["preflight"] = {"rpc": "configure_sos_results_feed", "arguments": {"p_election_id": payload["p_election_id"], "p_enabled": False}, "require_source_url": SOURCE}
            print(json.dumps(request, indent=2))
    except (KeyError, ValueError, OSError, subprocess.TimeoutExpired):
        # Never print dependency exceptions or administrator token-bearing curl configuration.
        print("Admin operation rejected or failed; verify environment, acceptance, identity, window and current status", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

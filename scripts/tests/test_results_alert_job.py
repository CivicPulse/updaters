import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
import json

spec = importlib.util.spec_from_file_location("alert_job", Path(__file__).parents[1] / "results-alert-test.py")
job = importlib.util.module_from_spec(spec)
spec.loader.exec_module(job)


class AlertJobTests(unittest.TestCase):
    def test_verification_uses_only_email_secret_and_never_claims_source_work(self):
        rendered = job.manifest("test", "sha256:" + "a" * 64)
        spec = rendered["spec"]["template"]["spec"]
        self.assertFalse(spec["automountServiceAccountToken"])
        self.assertEqual(rendered["spec"]["backoffLimit"], 0)
        container = spec["containers"][0]
        self.assertEqual(container["args"], ["alert-test"])
        env = {entry["name"]: entry for entry in container["env"]}
        self.assertEqual(env["SOS_COLLECTOR_ENABLED"]["value"], "false")
        self.assertNotIn("SUPABASE_SERVICE_ROLE_KEY", env)
        self.assertNotIn("R2_SECRET_ACCESS_KEY", env)
        self.assertEqual(env["CLOUDFLARE_EMAIL_API_TOKEN"]["valueFrom"]["secretKeyRef"]["name"], "results-alert-email")

    def test_repeated_runs_create_and_observe_distinct_fresh_jobs(self):
        commands = []
        outputs = []
        for suffix in ["fresh1", "fresh2"]:
            name = job.NAME + "-" + suffix
            def output(argv, **kwargs):
                commands.append(argv)
                if "create" in argv:
                    rendered = json.loads(kwargs["input"])
                    self.assertNotIn("name", rendered["metadata"])
                    self.assertEqual(rendered["metadata"]["generateName"], job.NAME + "-")
                    return json.dumps({"metadata": {"name": name, "uid": suffix}}).encode()
                if "get" in argv:
                    self.assertIn(name, argv)
                    return suffix.encode()
                self.assertEqual(argv[-1], "job/" + name)
                return b'{"delivery":"delivered","message_id":"receipt"}'
            with patch.object(job.sys, "argv", ["test", "--image-digest", "sha256:" + "a" * 64, "--apply"]), patch.object(job.subprocess, "check_output", side_effect=output), patch.object(job.subprocess, "run") as wait, patch("builtins.print") as printed:
                self.assertEqual(job.main(), 0)
                self.assertIn("job/" + name, wait.call_args.args[0])
                outputs.append(json.loads(printed.call_args.args[0]))
        self.assertNotEqual(outputs[0]["job_uid"], outputs[1]["job_uid"])
        self.assertEqual(sum("create" in command for command in commands), 2)

    def test_mutable_images_and_unrecognized_namespaces_are_rejected(self):
        for environment, digest in [("test", "latest"), ("production", "sha256:" + "a" * 64), ("prod", "sha256:" + "g" * 64)]:
            with self.assertRaises(ValueError):
                job.manifest(environment, digest)


if __name__ == "__main__":
    unittest.main()

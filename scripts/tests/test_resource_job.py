import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
import json

spec = importlib.util.spec_from_file_location("resource_job", Path(__file__).resolve().parents[1] / "results-resource-job.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ResourceJobTests(unittest.TestCase):
    def test_exec_is_test_only_and_uses_offline_auxiliary_not_daemon(self):
        argv=module.command("results-collector-abc")
        self.assertIn("civpulse-test",argv)
        self.assertIn("/usr/local/bin/civpulse-measure",argv)
        self.assertIn("--pipeline",argv)
        self.assertEqual(argv[argv.index("--scratch-root")+1], "/tmp")
        self.assertNotIn("once",argv)
        with self.assertRaises(ValueError):
            module.command("../../prod")

    def test_rejects_wrong_or_restarted_runtime_digest(self):
        digest="sha256:"+"a"*64
        obj={"spec":{"containers":[{"name":"collector","image":"registry.digitalocean.com/hatchk8s/civpulse-results-collector@"+digest,"resources":{},"envFrom":[{"configMapRef":{"name":"reviewed-config"}}]}]},"status":{"containerStatuses":[{"name":"collector","imageID":"containerd://registry.digitalocean.com/hatchk8s/civpulse-results-collector@"+digest,"ready":True,"restartCount":0}]}}
        with patch.object(module.subprocess,"check_output",side_effect=[json.dumps(obj).encode(),json.dumps({"data":{"SOS_COLLECTOR_ENABLED":"false"}}).encode()]):
            module.verify("pod",digest)
        with patch.object(module.subprocess,"check_output",return_value=json.dumps(obj).encode()):
            with self.assertRaises(ValueError):
                module.verify("pod","sha256:"+"b"*64)
        obj["status"]["containerStatuses"][0]["restartCount"]=1
        with patch.object(module.subprocess,"check_output",return_value=json.dumps(obj).encode()):
            with self.assertRaises(ValueError):
                module.verify("pod",digest)

import json
from pathlib import Path
from tempfile import TemporaryDirectory
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import unittest
import urllib.error
import urllib.parse
import urllib.request

from server import ApiError, Controller, make_handler, validate_device


class AdminTests(unittest.TestCase):
    def setUp(self):
        self.state = {"running": False, "fault": False, "direction": "forward", "voltage": 1.0, "frequency_hz": 8.0, "lease_ms": 500, "uptime_ms": 1000}
        self.calls = []
        owner = self

        class Device(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def reply(self, status=200, data=None):
                body = json.dumps(data or owner.state).encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                owner.calls.append((self.path, None))
                self.reply()

            def do_POST(self):
                data = urllib.parse.parse_qs(self.rfile.read(int(self.headers.get("Content-Length", "0"))).decode())
                owner.calls.append((self.path, data))
                if self.path == "/api/config":
                    owner.state.update(voltage=float(data["voltage"][0]), frequency_hz=float(data["frequency_hz"][0]))
                    if "direction" in data:
                        direction = data["direction"][0]
                        if direction != owner.state["direction"]:
                            owner.state["running"] = False
                        owner.state["direction"] = direction
                elif self.path == "/api/start":
                    owner.state["running"] = True
                elif self.path == "/api/stop":
                    owner.state["running"] = False
                self.reply()

        self.upstream = ThreadingHTTPServer(("127.0.0.1", 0), Device)
        self.upstream_thread = threading.Thread(target=self.upstream.serve_forever, daemon=True)
        self.upstream_thread.start()
        self.directory = TemporaryDirectory()
        self.controller = Controller(f"http://127.0.0.1:{self.upstream.server_port}", Path(self.directory.name) / "state.json")
        self.admin = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(self.controller))
        self.admin_thread = threading.Thread(target=self.admin.serve_forever, daemon=True)
        self.admin_thread.start()
        self.base = f"http://127.0.0.1:{self.admin.server_port}"

    def tearDown(self):
        self.admin.shutdown()
        self.upstream.shutdown()
        self.admin.server_close()
        self.upstream.server_close()
        self.admin_thread.join()
        self.upstream_thread.join()
        self.directory.cleanup()

    def request(self, path, data=None, headers=None):
        body = None if data is None else json.dumps(data).encode()
        request = urllib.request.Request(self.base + path, data=body, headers=headers or {})
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            with opener.open(request, timeout=2) as response:
                return response.status, json.loads(response.read())
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read())

    def test_configuration_roundtrip_does_not_start_motor(self):
        status, data = self.request("/api/config", {"voltage": .7, "frequency_hz": 10})
        self.assertEqual(status, 200)
        self.assertEqual(data["voltage"], .7)
        self.assertFalse(data["running"])
        self.assertEqual(self.request("/api/status")[1]["frequency_hz"], 10)

    def test_encoder_diagnostics_preserve_missing_and_live_samples(self):
        self.state["encoder"] = {"online": False, "angle_deg": None, "raw_angle": None, "magnet": "unknown", "error": "nack"}
        status, data = self.request("/api/status")
        self.assertEqual(status, 200)
        self.assertIsNone(data["encoder"]["angle_deg"])
        self.state["encoder"] = {"online": True, "angle_deg": 180.0, "raw_angle": 2048, "magnet": "ok", "paused": False, "age_ms": 10}
        status, data = self.request("/api/status")
        self.assertEqual(data["encoder"], self.state["encoder"])
        self.assertFalse(data["running"])

    def test_start_and_stop_are_explicit_forwarded_operations(self):
        self.assertTrue(self.request("/api/start", {"duration_ms": 3000})[1]["running"])
        self.assertFalse(self.request("/api/stop", {})[1]["running"])
        self.assertEqual(self.calls[0], ("/api/start", {"duration_ms": ["3000"]}))

    def test_invalid_values_never_reach_device(self):
        for voltage in (.49, 2.01, float("nan"), True):
            self.assertEqual(self.request("/api/config", {"voltage": voltage, "frequency_hz": 8})[0], 400)
        self.assertEqual(self.request("/api/start", {"duration_ms": -1})[0], 400)
        self.assertEqual(self.calls, [])

    def test_upper_bounds_and_reverse_are_forwarded(self):
        status, data = self.request("/api/config", {"voltage": 2, "frequency_hz": 100, "direction": "reverse"})
        self.assertEqual(status, 200)
        self.assertEqual(data["direction"], "reverse")
        self.assertFalse(data["running"])
        self.assertEqual(self.calls[-1][1]["direction"], ["reverse"])

    def test_direction_change_stops_but_omitting_direction_preserves_it(self):
        self.state["running"] = True
        _, data = self.request("/api/config", {"voltage": 1, "frequency_hz": 8, "direction": "reverse"})
        self.assertFalse(data["running"])
        _, data = self.request("/api/config", {"voltage": .5, "frequency_hz": 5})
        self.assertEqual(data["direction"], "reverse")

    def test_invalid_direction_and_frequency_are_rejected_before_forward(self):
        self.assertEqual(self.request("/api/config", {"voltage": 1, "frequency_hz": -.01})[0], 400)
        self.assertEqual(self.request("/api/config", {"voltage": 1, "frequency_hz": 100.1})[0], 400)
        self.assertEqual(self.request("/api/config", {"voltage": 1, "frequency_hz": 8, "direction": "wrong"})[0], 400)
        self.assertEqual(self.calls, [])

    def test_zero_frequency_is_forwarded_without_starting(self):
        status, data = self.request("/api/config", {"voltage": .5, "frequency_hz": 0})
        self.assertEqual(status, 200)
        self.assertEqual(data["frequency_hz"], 0)
        self.assertFalse(data["running"])

    def test_other_website_cannot_issue_control_request(self):
        self.assertEqual(self.request("/api/stop", {}, {"Origin": "http://other.invalid"})[0], 403)
        self.assertEqual(self.calls, [])

    def test_device_setting_is_verified_and_persisted(self):
        status, data = self.request("/api/settings", {"device_url": self.controller.device})
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(self.controller.state_file.read_text())["device_url"], data["device_url"])

    def test_cannot_change_device_during_running_session(self):
        self.state["running"] = True
        self.assertEqual(self.request("/api/settings", {"device_url": "http://192.168.0.103"})[0], 409)

    def test_device_url_must_be_private_http_address(self):
        for url in ("https://192.168.0.1", "http://8.8.8.8", "http://user:pass@192.168.0.1", "http://[bad", "not-an-ip"):
            with self.assertRaises(ApiError):
                validate_device(url)


if __name__ == "__main__":
    unittest.main()

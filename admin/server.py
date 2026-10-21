#!/usr/bin/env python3
"""Local motor management UI; uses only the Python standard library."""
import argparse
import ipaddress
import json
import math
import os
from pathlib import Path
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parent
DEFAULT_STATE = ROOT.parent / ".local" / "motor-admin.json"


def render_panel():
    """Serve the same offline viewer assets bundled by the firmware build."""
    folder = ROOT / "wiring"
    template = (ROOT / "index.html").read_text()
    if template.count("<!--WIRING_SECTION-->") != 1:
        raise RuntimeError("management panel section marker missing or duplicated")
    template = template.replace("<!--WIRING_SECTION-->", (folder / "section.html").read_text())
    css = (folder / "viewer.css").read_text()
    design = (folder / "design.json").read_text().replace("<", "\\u003c")
    script = (folder / "viewer.bundle.js").read_text()
    assets = (f"<style>{css}</style>"
              f'<script type="application/json" id="wiring-design">{design}</script>'
              f"<script>{script}</script>")
    if template.count("<!--WIRING_ASSETS-->") != 1:
        raise RuntimeError("management panel asset marker missing or duplicated")
    return template.replace("<!--WIRING_ASSETS-->", assets).encode()


class ApiError(Exception):
    def __init__(self, status, message):
        super().__init__(message)
        self.status = status


def validate_device(value):
    if not isinstance(value, str):
        raise ApiError(400, "设备地址必须是字符串")
    try:
        parsed = urllib.parse.urlsplit(value if "://" in value else "http://" + value)
        address = ipaddress.IPv4Address(parsed.hostname or "")
        port = parsed.port or 80
    except ValueError as error:
        raise ApiError(400, "请输入 ESP32-C3 的局域网 IPv4 地址") from error
    if (parsed.scheme != "http" or not address.is_private or address.is_link_local
            or address.is_multicast or address.is_unspecified or parsed.username
            or parsed.password or parsed.path not in ("", "/") or parsed.query or parsed.fragment):
        raise ApiError(400, "只支持局域网 HTTP 设备地址，不包含路径或账号")
    return f"http://{address}" + (f":{port}" if port != 80 else "")


def validate_parameters(data):
    if not {"voltage", "frequency_hz"}.issubset(data) or set(data) - {"voltage", "frequency_hz", "direction"}:
        raise ApiError(400, "需要 voltage 和 frequency_hz，direction 可选")
    for key, low, high in [("voltage", .5, 2), ("frequency_hz", 0, 100)]:
        value = data[key]
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or not low <= value <= high:
            raise ApiError(400, "幅值范围为 0.5–2V，电角频率范围为 0–100Hz")
    if "direction" in data and data["direction"] not in ("forward", "reverse"):
        raise ApiError(400, "direction 只能是 forward 或 reverse")
    return data


class Controller:
    def __init__(self, device, state_file=DEFAULT_STATE):
        self.device = validate_device(device)
        self.state_file = Path(state_file)
        self.lock = threading.Lock()

    def forward(self, path, data=None, device=None):
        with self.lock:
            target = device or self.device
        body = None if data is None else urllib.parse.urlencode(data).encode()
        request = urllib.request.Request(target + path, data=body)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            with opener.open(request, timeout=.7) as response:
                result = json.loads(response.read(4096))
        except urllib.error.HTTPError as error:
            try:
                message = json.loads(error.read(4096)).get("error", "设备拒绝请求")
            except (ValueError, AttributeError):
                message = "设备拒绝请求"
            raise ApiError(error.code, message) from error
        except (OSError, ValueError, urllib.error.URLError) as error:
            raise ApiError(502, "无法连接设备，请检查 ESP32-C3 电源、Wi-Fi 和 IP 地址") from error
        if not isinstance(result, dict):
            raise ApiError(502, "设备返回的状态格式不正确")
        return result

    def settings(self):
        with self.lock:
            return {"device_url": self.device}

    def set_device(self, value):
        target = validate_device(value)
        with self.lock:
            previous = self.device
        if target != previous:
            try:
                if self.forward("/api/status").get("running"):
                    raise ApiError(409, "请先停止电机，再切换设备")
            except ApiError as error:
                if error.status != 502:
                    raise
        status = self.forward("/api/status", device=target)
        if not {"running", "fault", "voltage", "frequency_hz"}.issubset(status):
            raise ApiError(502, "目标设备不是预期的电机控制服务")
        with self.lock:
            self.device = target
            self.state_file.parent.mkdir(parents=True, exist_ok=True)
            temporary = self.state_file.with_suffix(".tmp")
            temporary.write_text(json.dumps({"device_url": target}, indent=2) + "\n")
            os.replace(temporary, self.state_file)
        return {"device_url": target, "status": status}


def make_handler(controller):
    class Handler(BaseHTTPRequestHandler):
        server_version = "MotorAdmin/1.0"

        def log_message(self, *_):
            pass

        def reply(self, status, data, content_type="application/json; charset=utf-8"):
            body = data if isinstance(data, bytes) else json.dumps(data, ensure_ascii=False, allow_nan=False).encode()
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                self.wfile.write(body)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def check_origin(self):
            host = self.headers.get("Host", "")
            allowed = {f"127.0.0.1:{self.server.server_port}", f"localhost:{self.server.server_port}"}
            if host not in allowed:
                raise ApiError(403, "只接受本机访问")
            origin = self.headers.get("Origin")
            if origin and origin != "http://" + host:
                raise ApiError(403, "拒绝其他网站发起的控制请求")

        def do_GET(self):
            try:
                self.check_origin()
                if self.path == "/":
                    self.reply(200, render_panel(), "text/html; charset=utf-8")
                elif self.path == "/api/settings":
                    self.reply(200, controller.settings())
                elif self.path == "/api/status":
                    self.reply(200, controller.forward("/api/status"))
                else:
                    raise ApiError(404, "接口不存在")
            except ApiError as error:
                self.reply(error.status, {"error": str(error)})

        def do_POST(self):
            try:
                self.check_origin()
                if self.headers.get("Transfer-Encoding") or len(self.headers.get_all("Content-Length", [])) > 1:
                    raise ApiError(400, "请求格式不正确")
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                except ValueError as error:
                    raise ApiError(400, "请求长度不正确") from error
                if not 0 <= length <= 1024:
                    raise ApiError(413, "请求内容过大")
                try:
                    data = json.loads(self.rfile.read(length)) if length else {}
                except (ValueError, UnicodeError) as error:
                    raise ApiError(400, "请求内容必须为 JSON 对象") from error
                if not isinstance(data, dict):
                    raise ApiError(400, "请求内容必须为 JSON 对象")
                if self.path == "/api/settings":
                    if set(data) != {"device_url"}:
                        raise ApiError(400, "需要 device_url 参数")
                    result = controller.set_device(data["device_url"])
                elif self.path == "/api/config":
                    result = controller.forward(self.path, validate_parameters(data))
                elif self.path == "/api/haptic/config":
                    if set(data) != {"mode", "strength", "step_deg"}:
                        raise ApiError(400, "需要 mode、strength、step_deg")
                    if data["mode"] not in ("free", "detents", "switch", "selector", "spring", "ratchet_forward", "ratchet_reverse"):
                        raise ApiError(400, "未知旋钮模式")
                    for key, low, high in (("strength", 0.1, 1), ("step_deg", 5, 90)):
                        value = data[key]
                        if isinstance(value, bool) or not isinstance(value, (float, int)) or not math.isfinite(value) or not low <= value <= high:
                            raise ApiError(400, "旋钮参数超出范围")
                    result = controller.forward(self.path, data)
                elif self.path in ("/api/start", "/api/haptic/start"):
                    if set(data) - {"duration_ms"}:
                        raise ApiError(400, "未知启动参数")
                    duration = data.get("duration_ms", 3000)
                    if isinstance(duration, bool) or not isinstance(duration, int) or not 0 <= duration <= 30000:
                        raise ApiError(400, "duration_ms 须为 0–30000 的整数")
                    result = controller.forward(self.path, {"duration_ms": duration})
                elif self.path in ("/api/stop", "/api/keepalive", "/api/haptic/calibrate"):
                    if data:
                        raise ApiError(400, "此接口不接受额外参数")
                    result = controller.forward(self.path, {})
                else:
                    raise ApiError(404, "接口不存在")
                self.reply(200, result)
            except ApiError as error:
                self.reply(error.status, {"error": str(error)})

    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--device", help="ESP32-C3 LAN URL")
    parser.add_argument("--port", type=int, default=8766)
    args = parser.parse_args()
    saved = {}
    if DEFAULT_STATE.exists():
        try:
            saved = json.loads(DEFAULT_STATE.read_text())
        except ValueError:
            pass
    controller = Controller(args.device or saved.get("device_url", "http://192.168.0.102"))
    server = ThreadingHTTPServer(("127.0.0.1", args.port), make_handler(controller))
    server.daemon_threads = True
    print(f"Motor admin: http://127.0.0.1:{server.server_port}/", flush=True)
    print(f"Device: {controller.device}; motor stays stopped until explicitly started.", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
